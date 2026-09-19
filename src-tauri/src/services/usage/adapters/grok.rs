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

    // Issue #1745 phase 2 step 17: grok migrated to the outcome seam.
    // Missing credential → `NoCredential` (gate drops, as before).
    // 401/403 → `Rejected` ("Invalid API key" affordance). 429 →
    // `RateLimited`. Client-build / transport / non-2xx / parse →
    // `Unavailable`. The ladder stays hand-rolled (the kimi precedent —
    // the shared driver cannot carry the prepaid-balance reading).
    fn fetch(&self, _accounts: &[ProviderAccount]) -> UsageOutcome {
        crate::services::usage::grok_usage()
    }
}
