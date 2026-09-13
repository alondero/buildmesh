//! Freebuff native adapter — self-authenticates through its CLI-managed
//! credentials file, detection-gated on the `freebuff` harness.
//!
//! Promoted first (issue #1657 step 2) to prove the seam is real: the fetch
//! path lives fully in [`crate::services::freebuff_usage`] and imports shared
//! wire types from [`crate::services::usage::types`] only — never fetcher
//! internals out of the legacy `usage.rs` module.

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::UsageAdapter;
use crate::services::usage::outcome::UsageOutcome;

/// Drop-in [`UsageAdapter`] for `freebuff`.
pub(crate) struct FreebuffAdapter;

impl UsageAdapter for FreebuffAdapter {
    fn id(&self) -> &'static str {
        "freebuff"
    }

    fn native_harness(&self) -> Option<&'static str> {
        Some("freebuff")
    }

    // TODO(#1745 phase 2): migrate `freebuff_usage` to return `UsageOutcome`
    // directly so the adapter is fully seam-owned.
    fn fetch(&self, _accounts: &[ProviderAccount]) -> UsageOutcome {
        crate::services::freebuff_usage::freebuff_usage().into()
    }
}
