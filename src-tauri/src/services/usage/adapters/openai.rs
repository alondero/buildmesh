//! OpenAI keyed adapter — Organization Costs endpoint (admin-scoped;
//! project keys degrade through the fetcher's normal logged-in/detail
//! envelope). Card always visible.

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::{api_key_for, UsageAdapter};
use crate::services::usage::outcome::UsageOutcome;

/// Drop-in [`UsageAdapter`] for `openai`.
pub(crate) struct OpenaiAdapter;

impl UsageAdapter for OpenaiAdapter {
    fn id(&self) -> &'static str {
        "openai"
    }

    // Issue #1745 phase 2 step 10: openai migrated to the outcome seam.
    // Empty key → `NoCredential`. 401/403 on inference → `Rejected`. 429 →
    // `RateLimited`. 401/403 on costs (sk-proj- contract per ADR-0026 §2)
    // → `Degraded` (logged_in: true with `detail` hint). Other → `Unavailable`.
    fn fetch(&self, accounts: &[ProviderAccount]) -> UsageOutcome {
        crate::services::usage::openai_usage(api_key_for(accounts, "openai").unwrap_or(""))
    }
}
