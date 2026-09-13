//! MiniMax keyed adapter — credential comes from the effective account
//! snapshot, never from disk. Card always visible so the key editor stays
//! reachable.

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::{api_key_for, UsageAdapter};
use crate::services::usage::outcome::UsageOutcome;

/// Drop-in [`UsageAdapter`] for `minimax`.
pub(crate) struct MinimaxAdapter;

impl UsageAdapter for MinimaxAdapter {
    fn id(&self) -> &'static str {
        "minimax"
    }

    // Issue #1745 phase 2 step 1: minimax is the first provider fully
    // migrated to the outcome seam. Empty key → `NoCredential` (gate
    // drops). Non-empty key → routes through the shared driver, which
    // classifies 401/403 as `Rejected` (the "Invalid API key"
    // affordance) — matching MiniMax's keyed siblings
    // (Kimi/OpenRouter/OpenAI/DeepSeek).
    fn fetch(&self, accounts: &[ProviderAccount]) -> UsageOutcome {
        crate::services::usage::minimax_usage(api_key_for(accounts, "minimax").unwrap_or(""))
    }
}
