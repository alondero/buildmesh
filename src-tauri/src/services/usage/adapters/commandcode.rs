//! CommandCode native adapter — self-authenticates via its CLI credential,
//! detection-gated on the `commandcode` harness.

use crate::preferences::ProviderAccount;
use crate::services::usage::adapter::UsageAdapter;
use crate::services::usage::outcome::UsageOutcome;

/// Drop-in [`UsageAdapter`] for `commandcode`.
pub(crate) struct CommandcodeAdapter;

impl UsageAdapter for CommandcodeAdapter {
    fn id(&self) -> &'static str {
        "commandcode"
    }

    fn native_harness(&self) -> Option<&'static str> {
        Some("commandcode")
    }

    // TODO(#1745 phase 2): migrate `commandcode_usage` to return `UsageOutcome`
    // directly so its hand-rolled status ladder centralises in the shared
    // driver. Until then the shim preserves the wire triple.
    fn fetch(&self, _accounts: &[ProviderAccount]) -> UsageOutcome {
        crate::services::usage::commandcode_usage().into()
    }
}
