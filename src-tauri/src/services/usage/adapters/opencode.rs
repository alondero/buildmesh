//! OpenCode native adapter — Buildmesh-owned OAuth credential +
//! local `opencode.db` SQLite fallback. Detection-gated on `opencode`.
//!
//! The live fetcher still lives in the legacy `usage.rs` module in this
//! commit (thin wrapper); the follow-up moves `opencode_usage_impl`,
//! the refresh logic and the SQLite fallback here behind the seam so
//! `usage.rs` keeps only the cached-read path.

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::UsageAdapter;
use crate::services::usage::outcome::UsageOutcome;

/// Drop-in [`UsageAdapter`] for `opencode`.
pub(crate) struct OpencodeAdapter;

impl UsageAdapter for OpencodeAdapter {
    fn id(&self) -> &'static str {
        "opencode"
    }

    fn native_harness(&self) -> Option<&'static str> {
        Some("opencode")
    }

    // TODO(#1745 phase 2): migrate `opencode_usage` to return `UsageOutcome`
    // directly so the retry-on-401 logic can switch from "substring-match
    // the wire error string for `401`" (fragile) to "outcome is `Rejected`".
    // Today the shim preserves the wire triple.
    fn fetch(&self, _accounts: &[ProviderAccount]) -> UsageOutcome {
        crate::services::usage::opencode_usage().into()
    }
}
