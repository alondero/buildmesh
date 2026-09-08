//! Anthropic (Claude Code) native adapter — self-authenticates via
//! `~/.claude/.credentials.json`, detection-gated on the `anthropic` harness.

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::UsageAdapter;
use crate::services::usage::types::ProviderUsage;

/// Drop-in [`UsageAdapter`] for `anthropic`.
pub(crate) struct AnthropicAdapter;

impl UsageAdapter for AnthropicAdapter {
    fn id(&self) -> &'static str {
        "anthropic"
    }

    fn native_harness(&self) -> Option<&'static str> {
        Some("anthropic")
    }

    fn fetch(&self, _accounts: &[ProviderAccount]) -> ProviderUsage {
        crate::services::usage::anthropic_usage()
    }
}
