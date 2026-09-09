//! CommandCode native adapter — self-authenticates via its CLI credential,
//! detection-gated on the `commandcode` harness.

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::UsageAdapter;
use crate::services::usage::types::ProviderUsage;

/// Drop-in [`UsageAdapter`] for `commandcode`.
pub(crate) struct CommandcodeAdapter;

impl UsageAdapter for CommandcodeAdapter {
    fn id(&self) -> &'static str {
        "commandcode"
    }

    fn native_harness(&self) -> Option<&'static str> {
        Some("commandcode")
    }

    fn fetch(&self, _accounts: &[ProviderAccount]) -> ProviderUsage {
        crate::services::usage::commandcode_usage()
    }
}
