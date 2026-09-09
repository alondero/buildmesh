//! OpenAI keyed adapter — Organization Costs endpoint (admin-scoped;
//! project keys degrade through the fetcher's normal logged-in/detail
//! envelope). Card always visible.

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::{api_key_for, UsageAdapter};
use crate::services::usage::types::ProviderUsage;

/// Drop-in [`UsageAdapter`] for `openai`.
pub(crate) struct OpenaiAdapter;

impl UsageAdapter for OpenaiAdapter {
    fn id(&self) -> &'static str {
        "openai"
    }

    fn fetch(&self, accounts: &[ProviderAccount]) -> ProviderUsage {
        crate::services::usage::openai_usage(api_key_for(accounts, "openai").unwrap_or(""))
    }
}
