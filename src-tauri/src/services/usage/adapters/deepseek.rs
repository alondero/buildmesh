//! DeepSeek keyed adapter — keyed cash-balance endpoint (issue #1127).
//! Card always visible.

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::{api_key_for, UsageAdapter};
use crate::services::usage::types::ProviderUsage;

/// Drop-in [`UsageAdapter`] for `deepseek`.
pub(crate) struct DeepseekAdapter;

impl UsageAdapter for DeepseekAdapter {
    fn id(&self) -> &'static str {
        "deepseek"
    }

    fn fetch(&self, accounts: &[ProviderAccount]) -> ProviderUsage {
        crate::services::usage::deepseek_usage(api_key_for(accounts, "deepseek").unwrap_or(""))
    }
}
