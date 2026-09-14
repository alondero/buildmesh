//! Grok (xAI) native adapter — self-authenticates via its CLI credential,
//! detection-gated on the `grok` harness.

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::UsageAdapter;
use crate::services::usage::outcome::UsageOutcome;

/// Drop-in [`UsageAdapter`] for `grok`.
pub(crate) struct GrokAdapter;

impl UsageAdapter for GrokAdapter {
    fn id(&self) -> &'static str {
        "grok"
    }

    fn native_harness(&self) -> Option<&'static str> {
        Some("grok")
    }

    // TODO(#1745 phase 2): migrate `grok_usage` out of the legacy fetcher
    // so its hand-rolled status ladder (no credential / 401 / 403 / 429 /
    // transport / parse) centralises in the shared driver. Today its
    // no-credential case correctly returns `logged_out()` (the row is dropped
    // by the gate) — preserve that on migration.
    fn fetch(&self, _accounts: &[ProviderAccount]) -> UsageOutcome {
        crate::services::usage::grok_usage().into()
    }
}
