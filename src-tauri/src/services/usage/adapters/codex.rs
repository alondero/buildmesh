//! Codex CLI native adapter — self-authenticates via `~/.codex/auth.json`
//! (`$CODEX_HOME` override + WSL fallback), detection-gated on `codex`.

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::UsageAdapter;
use crate::services::usage::types::ProviderUsage;

/// Drop-in [`UsageAdapter`] for `codex`.
pub(crate) struct CodexAdapter;

impl UsageAdapter for CodexAdapter {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn native_harness(&self) -> Option<&'static str> {
        Some("codex")
    }

    fn fetch(&self, _accounts: &[ProviderAccount]) -> ProviderUsage {
        crate::services::usage::codex_usage()
    }
}
