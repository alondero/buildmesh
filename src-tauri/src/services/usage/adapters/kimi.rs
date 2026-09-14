//! Kimi (Moonshot) keyed wallet adapter — cash-balance endpoint rather
//! than plan windows. Card always visible.

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::{api_key_for, UsageAdapter};
use crate::services::usage::outcome::UsageOutcome;

/// Drop-in [`UsageAdapter`] for `kimi`.
pub(crate) struct KimiAdapter;

impl UsageAdapter for KimiAdapter {
    fn id(&self) -> &'static str {
        "kimi"
    }

    // Issue #1745 phase 2 step 8: kimi migrated to the outcome seam.
    // Empty key → `NoCredential`. 401/403 → `Rejected` ("Invalid API key"
    // affordance). 429 → `RateLimited`. Other → `Unavailable`.
    fn fetch(&self, accounts: &[ProviderAccount]) -> UsageOutcome {
        crate::services::usage::kimi_usage(api_key_for(accounts, "kimi").unwrap_or(""))
    }
}
