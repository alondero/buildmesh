//! Cursor native adapter — self-authenticates via the Cursor CLI auth
//! sources, detection-gated on the `cursor` harness.

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::UsageAdapter;
use crate::services::usage::types::ProviderUsage;

/// Drop-in [`UsageAdapter`] for `cursor`.
pub(crate) struct CursorAdapter;

impl UsageAdapter for CursorAdapter {
    fn id(&self) -> &'static str {
        "cursor"
    }

    fn native_harness(&self) -> Option<&'static str> {
        Some("cursor")
    }

    fn fetch(&self, _accounts: &[ProviderAccount]) -> ProviderUsage {
        crate::services::usage::cursor_usage()
    }
}
