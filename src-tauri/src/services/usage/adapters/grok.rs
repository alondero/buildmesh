//! Grok (xAI) native adapter — self-authenticates via its CLI credential,
//! detection-gated on the `grok` harness.

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::UsageAdapter;
use crate::services::usage::types::ProviderUsage;

/// Drop-in [`UsageAdapter`] for `grok`.
pub(crate) struct GrokAdapter;

impl UsageAdapter for GrokAdapter {
    fn id(&self) -> &'static str {
        "grok"
    }

    fn native_harness(&self) -> Option<&'static str> {
        Some("grok")
    }

    fn fetch(&self, _accounts: &[ProviderAccount]) -> ProviderUsage {
        crate::services::usage::grok_usage()
    }
}
