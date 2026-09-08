//! MiniMax keyed adapter — credential comes from the effective account
//! snapshot, never from disk. Card always visible so the key editor stays
//! reachable.

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::{api_key_for, UsageAdapter};
use crate::services::usage::types::ProviderUsage;

/// Drop-in [`UsageAdapter`] for `minimax`.
pub(crate) struct MinimaxAdapter;

impl UsageAdapter for MinimaxAdapter {
    fn id(&self) -> &'static str {
        "minimax"
    }

    fn fetch(&self, accounts: &[ProviderAccount]) -> ProviderUsage {
        crate::services::usage::minimax_usage(api_key_for(accounts, "minimax").unwrap_or(""))
    }
}
