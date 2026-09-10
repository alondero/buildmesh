//! Antigravity (`agy`) native adapter — token lives in
//! `<agy_dir>/antigravity-oauth-token` (current CLI) with a
//! `gemini:antigravity` keyring fallback, detection-gated on the `agy` harness.

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::UsageAdapter;
use crate::services::usage::types::ProviderUsage;

/// Drop-in [`UsageAdapter`] for `agy`.
pub(crate) struct AgyAdapter;

impl UsageAdapter for AgyAdapter {
    fn id(&self) -> &'static str {
        "agy"
    }

    fn native_harness(&self) -> Option<&'static str> {
        Some("agy")
    }

    fn fetch(&self, _accounts: &[ProviderAccount]) -> ProviderUsage {
        crate::services::usage::agy_usage()
    }
}
