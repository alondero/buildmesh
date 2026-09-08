//! Kimi (Moonshot) keyed wallet adapter — cash-balance endpoint rather
//! than plan windows. Card always visible.

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::{api_key_for, UsageAdapter};
use crate::services::usage::types::ProviderUsage;

/// Drop-in [`UsageAdapter`] for `kimi`.
pub(crate) struct KimiAdapter;

impl UsageAdapter for KimiAdapter {
    fn id(&self) -> &'static str {
        "kimi"
    }

    fn fetch(&self, accounts: &[ProviderAccount]) -> ProviderUsage {
        crate::services::usage::kimi_usage(api_key_for(accounts, "kimi").unwrap_or(""))
    }
}
