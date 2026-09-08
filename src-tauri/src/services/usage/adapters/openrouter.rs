//! OpenRouter keyed adapter — cash-balance endpoint. Card always visible
//! so a revoked key keeps its row (the gate keeps `logged_out` envelopes
//! when a key is configured, so the UI can render "Invalid API key").

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::{api_key_for, UsageAdapter};
use crate::services::usage::types::ProviderUsage;

/// Drop-in [`UsageAdapter`] for `openrouter`.
pub(crate) struct OpenrouterAdapter;

impl UsageAdapter for OpenrouterAdapter {
    fn id(&self) -> &'static str {
        "openrouter"
    }

    fn fetch(&self, accounts: &[ProviderAccount]) -> ProviderUsage {
        crate::services::usage::openrouter_usage(api_key_for(accounts, "openrouter").unwrap_or(""))
    }
}
