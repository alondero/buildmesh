//! DeepSeek keyed adapter — keyed cash-balance endpoint (issue #1127).
//! Card always visible.

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::{api_key_for, UsageAdapter};
use crate::services::usage::outcome::UsageOutcome;

/// Drop-in [`UsageAdapter`] for `deepseek`.
pub(crate) struct DeepseekAdapter;

impl UsageAdapter for DeepseekAdapter {
    fn id(&self) -> &'static str {
        "deepseek"
    }

    // Issue #1745 phase 2 step 11: deepseek migrated to the outcome seam.
    // Empty key → `NoCredential`. 401/403 → `Rejected`. 429 → `RateLimited`.
    // Other → `Unavailable`.
    fn fetch(&self, accounts: &[ProviderAccount]) -> UsageOutcome {
        crate::services::usage::deepseek_usage(api_key_for(accounts, "deepseek").unwrap_or(""))
    }
}
