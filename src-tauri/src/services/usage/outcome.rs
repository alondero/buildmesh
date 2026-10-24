//! Internal failure taxonomy for the UsageAdapter seam (issue #1745).
//!
//! `UsageOutcome` is the only return type of [`super::adapter::UsageAdapter::fetch`].
//! The wire shape [`super::types::ProviderUsage`] is a projection owned in exactly
//! one place ([`UsageOutcome::into_usage`]) — adapters cannot bypass it, so the
//! same underlying situation cannot be encoded three different ways across
//! providers.
//!
//! What lives here:
//! - The [`UsageOutcome`] enum (`Reading`, `Enriched`, `NoCredential`,
//!   `Rejected`, `RateLimited`, `Unavailable`, `Degraded`,
//!   `ManagedExternally`).
//! - The [`UsageReading`] payload a successful read produces: windowed quota,
//!   an optional prepaid balance, meters and a detail line. A
//!   balance-style reading is representable *alongside* windowed quota, not
//!   instead of it.
//! - The [`UsageEnrichmentPatch`] a best-effort second read contributes to
//!   the reading it augments.
//! - The [`ResponsePolicy`] the shared fetch driver uses to word a failed
//!   response.
//! - The [`UsageOutcome::into_usage`] projection — the only site that mints a
//!   [`super::types::ProviderUsage`] from an outcome.
//! - The [`UsageOutcome::keep`] predicate that [`crate::commands::usage::assemble_meters`]
//!   uses to decide which rows surface on the Providers page.
//!
//! What does NOT live here: `ts_rs::TS` derives (ADR-0009 — wire shapes only).
//! `UsageOutcome` is `pub(crate)` and never serialised; the projection is the
//! boundary between the internal taxonomy and the externally-stable wire.

use super::types::{
    logged_out, unavailable, BillingBalance, ProviderUsage, UsageMeter, UsageWindow,
};
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
/// Each variant has a constructor, so `cargo check --all-targets` reports no
/// never-constructed variant: `NoCredential` on each adapter's
/// missing-credential path, `ManagedExternally` by the Anthropic adapter's
/// cloud/API-platform arm (issue #1758), and the rest through the
/// `UsageOutcome::*` constructors above (issue #2450).
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
    /// A reading produced by a primary read plus a second, best-effort read
    /// (issue #2528 step 1).
    ///
    /// The second read is *enrichment*: it does not fail the reading.
    /// Whatever it contributed is already merged into `reading`, so this
    /// variant projects like the bare reading it wraps — the panel renders
    /// the same bytes either way, which the projection test in
    /// `outcome_tests.rs` pins.
    ///
    /// The variant exists so the outcome records that a second read was
    /// attempted.
    /// Without it the enrichment would be an invisible side effect of the
    /// fetch rather than a modelled part of its result, and a provider that
    /// needed one would have to bolt an extra HTTP call onto its own.
    Enriched {
        /// The primary read's outcome, already carrying the enrichment's
        /// contribution.
        reading: Box<UsageOutcome>,
    },
    /// No credential on disk / not logged in / mechanism is not a subscription
    /// (e.g. an API-key provider with no key, or Muse Code without an OAuth
    /// login). Maps to `logged_in: false` so the gate drops the row.
    NoCredential { hint: String },
    /// Credential present but the provider rejected it (HTTP 401/403 with the
    /// adapter's [`ResponsePolicy`] rejection copy). Renders the
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

/// The payload a successful usage read produces.
///
/// This is the shape the shared driver's parse step returns, and it is a
/// struct rather than a bare tuple for one reason: a balance-style reading
/// (Kimi's wallet, Grok's prepaid balance) must be representable
/// *alongside* windowed quota (Codex, Anthropic), not instead of it. The
/// pre-#2528 driver returned `(windows, detail)` and forced a balance-only
/// provider to hand-roll its own HTTP ladder; a provider that
/// reports both a monthly meter and a prepaid balance now fits the same
/// shape.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct UsageReading {
    /// The provider's quota windows, in the order the panel renders them.
    pub windows: Vec<UsageWindow>,
    /// The provider's prepaid / cash balance, when it reports one.
    pub balance: Option<BillingBalance>,
    /// The provider's metered amounts.
    pub meters: Vec<UsageMeter>,
    /// A free-text detail line (plan name, extra credits, …).
    pub detail: Option<String>,
}

impl UsageReading {
    /// Project the reading onto the [`UsageOutcome::Reading`] variant.
    ///
    /// The reading has a single projection site, so an adapter has one way
    /// to encode the same payload.
    pub(crate) fn into_outcome(self) -> UsageOutcome {
        UsageOutcome::Reading {
            windows: self.windows,
            balance: self.balance,
            meters: self.meters,
            detail: self.detail,
        }
    }
}

/// What a best-effort enrichment read contributed to the reading it
/// augments.
///
/// The patch is merged into the reading by the shared driver before the
/// outcome is built, so the reading and the patch stay in step — the
/// outcome carries no second copy of the reading that could fall out of
/// sync.
///
/// A successful enrichment *replaces* the primary read's balance and detail
/// line and *appends* its windows to the primary read's windows. The
/// replacement is deliberate: a provider that reports a monthly meter and a
/// prepaid balance shows the meter, not the balance — so the enrichment
/// that produced the meter also suppresses the balance.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct UsageEnrichmentPatch {
    /// Windows the enrichment added, appended after the primary read's
    /// windows in this order.
    pub windows: Vec<UsageWindow>,
    /// The balance the reading carries after the enrichment. `None` means
    /// the reading carries no balance.
    pub balance: Option<BillingBalance>,
    /// The detail line the reading carries after the enrichment.
    pub detail: Option<String>,
}

/// How the shared fetch driver words a failed response.
///
/// The driver owns the request → status → parse flow; what differs between
/// providers is how a failure is *worded*. Encoding those differences in
/// one policy value keeps the ladder itself shared instead of hand-rolled
/// per adapter — the gap issue #2528 step 1 exists to close.
///
/// The [`Default`] value reproduces the driver's original wording,
/// so an adapter that does not need custom copy keeps the shared text.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResponsePolicy {
    /// The remediation copy a 401/403 response maps to. `None` uses the
    /// default `"API error {status}: {body}"`.
    pub rejected_hint: Option<&'static str>,
    /// The copy a body-read failure maps to. `None` treats an unreadable
    /// body as empty and carries on with it.
    pub body_read_error: Option<&'static str>,
    /// How a non-2xx response body is rendered into the error reason.
    pub error_body: ErrorBody,
}

/// How a non-2xx response body is rendered into the error reason.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ErrorBody {
    /// Use the response body verbatim.
    #[default]
    Raw,
    /// Use the trimmed body, or `fallback` when the body is blank.
    TrimmedOr(&'static str),
}

impl ErrorBody {
    /// Render `body` into the detail half of an `"API error {code}: {detail}"`
    /// reason.
    pub(crate) fn render(self, body: &str) -> &str {
        match self {
            Self::Raw => body,
            Self::TrimmedOr(fallback) => {
                let trimmed = body.trim();
                if trimmed.is_empty() {
                    fallback
                } else {
                    trimmed
                }
            }
        }
    }
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
            // An enriched reading projects like the bare reading it wraps:
            // the enrichment is internal bookkeeping and does not change what
            // the panel renders.
            Self::Enriched { reading } => reading.into_usage(provider),
        }
    }

    /// Pure keep/drop predicate for [`crate::commands::usage::assemble_meters`].
    /// Centralising the decision in the outcome taxonomy means one table test
    /// covers all 8 variants; the gate in `assemble_meters` becomes a thin
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
            // An enriched reading is kept like the bare reading it
            // wraps.
            Self::Enriched { reading } => reading.keep(configured_keys, provider_id),
            Self::Reading { .. }
            | Self::RateLimited { .. }
            | Self::Unavailable { .. }
            | Self::Degraded { .. }
            | Self::ManagedExternally { .. } => true,
        }
    }

    /// True when the outcome carries a real reading of the provider's usage.
    ///
    /// [`Self::Enriched`] wraps a reading, so it counts: the last-known store
    /// records what the provider actually reported, and an enriched reading
    /// reports as much as the bare reading it wraps.
    pub(crate) fn is_reading(&self) -> bool {
        match self {
            Self::Reading { .. } => true,
            Self::Enriched { reading } => reading.is_reading(),
            Self::NoCredential { .. }
            | Self::Rejected { .. }
            | Self::RateLimited { .. }
            | Self::Unavailable { .. }
            | Self::Degraded { .. }
            | Self::ManagedExternally { .. } => false,
        }
    }

    /// True when the outcome is a transient failure — a rate limit or an
    /// unavailable response — for which a remembered reading is preferred
    /// over the live error.
    ///
    /// [`crate::commands::usage::assemble_meters`] reads this instead of
    /// listing the failure variants itself, so the taxonomy owns the
    /// classification. The match below has no wildcard arm, so a new
    /// variant fails to compile until the predicate covers it.
    ///
    /// [`Self::Enriched`] looks through the wrapper: an enriched reading is
    /// not transient, and a provider that wraps a failure in an enriched
    /// reading is classified by the outcome it wraps.
    pub(crate) fn is_transient(&self) -> bool {
        match self {
            Self::RateLimited { .. } | Self::Unavailable { .. } => true,
            Self::Enriched { reading } => reading.is_transient(),
            Self::Reading { .. }
            | Self::NoCredential { .. }
            | Self::Rejected { .. }
            | Self::Degraded { .. }
            | Self::ManagedExternally { .. } => false,
        }
    }
}

#[cfg(test)]
#[path = "outcome_tests.rs"]
mod tests;
