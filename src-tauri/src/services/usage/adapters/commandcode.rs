//! CommandCode native adapter — self-authenticates via its CLI credential,
//! detection-gated on the `commandcode` harness.

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::UsageAdapter;
use crate::services::usage::outcome::UsageOutcome;

/// Drop-in [`UsageAdapter`] for `commandcode`.
pub(crate) struct CommandcodeAdapter;

impl UsageAdapter for CommandcodeAdapter {
    fn id(&self) -> &'static str {
        "commandcode"
    }

    fn native_harness(&self) -> Option<&'static str> {
        Some("commandcode")
    }

    // Issue #1745 phase 2 step 14: commandcode migrated to the outcome
    // seam. Missing credential → `NoCredential`. 401/403 → `Rejected` with
    // the session-expired remediation. 429 → `RateLimited`. Other →
    // `Unavailable`. The dual-fetch quota + subscription-enrichment ladder
    // stays hand-rolled (the kimi precedent — the shared driver cannot
    // carry it); enrichment failures stay best-effort.
    fn fetch(&self, _accounts: &[ProviderAccount]) -> UsageOutcome {
        crate::services::usage::commandcode_usage()
    }
}
