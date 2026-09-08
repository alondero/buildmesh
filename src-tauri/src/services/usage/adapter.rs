//! The `UsageAdapter` seam (issue #1657).
//!
//! After the split the `usage` module keeps wire types ([`super::types`]) +
//! cache ([`super::cache`]) only. One trait sits between the catalog table
//! and per-provider adapters:
//!
//! ```text
//! usage module (types + cache only, deep: small interface, shared types)
//!              │ seam: UsageAdapter { id(), fetch(accounts) -> ProviderUsage }
//!    ┌─────────┼──────────┬──────────────┬─── …nth adapter
//! anthropic  minimax   opencode   freebuff
//! adapter    adapter   adapter    adapter
//! ```
//!
//! Catalog dispatch and `commands/usage.rs` ask the seam — never per-provider
//! lore. Adding provider N means adding one `adapters/<name>.rs` file and one
//! catalog entry (drop-in adapter to delete), not editing the fetcher module
//! AND the catalog entry.
//!
//! Two adapters already existed in spirit (freebuff, opencode-oauth DTO) and
//! were promoted here first to prove the seam is real; the rest migrate one
//! provider per commit. Thin wrappers that still delegate to the legacy
//! `usage.rs` fetchers are an intentional intermediate step — the catalog no
//! longer holds raw fn pointers, so the seam is the test surface even before
//! `usage.rs` reaches zero HTTP code.

use super::types::{ProviderUsage, UsageError, UsageWindow};
use crate::preferences::ProviderAccount;
use reqwest::blocking::{Client, RequestBuilder};
use std::time::Duration;

/// One first-class Usage Meter behind the seam.
///
/// - [`id`](UsageAdapter::id) is the provider account id (`"anthropic"`,
///   `"minimax"`, …) and the cache key.
/// - [`native_harness`](UsageAdapter::native_harness) is `Some(harness)` for
///   self-authenticating native meters (detection-gated card) and `None` for
///   keyed meters (card always visible; credential comes from `accounts`).
/// - [`fetch`](UsageAdapter::fetch) takes the effective account snapshot the
///   command already resolved — adapters never read preferences themselves.
pub(crate) trait UsageAdapter: Send + Sync {
    fn id(&self) -> &'static str;
    fn native_harness(&self) -> Option<&'static str> {
        None
    }
    fn fetch(&self, accounts: &[ProviderAccount]) -> ProviderUsage;
}

/// Resolve the non-empty API key for a keyed provider from the effective
/// account snapshot. `None` means "no credential configured" — keyed adapters
/// pass `""` through to their legacy fetcher, which returns the `logged_out`
/// envelope the card-assembly gate drops (vs `unavailable` for a present-but-
/// rejected key, which the gate keeps so the UI can render "Invalid API key").
pub(crate) fn api_key_for<'a>(
    accounts: &'a [ProviderAccount],
    provider_id: &str,
) -> Option<&'a str> {
    accounts
        .iter()
        .find(|account| account.id == provider_id)
        .and_then(|account| account.api_key.as_deref())
        .filter(|key| !key.is_empty())
}

/// One shared HTTP client for all adapters (issue #1657 step 6).
///
/// Previously every fetcher built its own `reqwest::blocking::Client` inline,
/// so tests could only intercept at the loopback-HTTP layer. Centralising
/// construction here is the first half of the transport injection: the second
/// half (a `fetch_json` seam taking a transport) can land per-adapter without
/// touching the call sites again. Timeout mirrors the Freebuff fetcher's 15s.
pub(crate) fn shared_client() -> Result<Client, String> {
    Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("Client error: {e}"))
}

/// Drives the shared request → status-check → parse flow. Callers reach this
/// only once a credential is confirmed present, so any failure here is reported
/// as logged-in-but-unavailable. `parse` maps a 2xx body to `(windows, detail)`.
///
/// Moved here from the `usage.rs` god-module so adapters import the driver
/// from the seam, never from the fetcher module. Behaviour is unchanged —
/// existing `parse_*` + loopback tests must pass unmodified after each
/// provider move (tests move files, not assertions).
pub(crate) fn fetch_usage(
    provider: &str,
    build_request: impl FnOnce(&Client) -> RequestBuilder,
    parse: impl FnOnce(&str) -> Result<(Vec<UsageWindow>, Option<String>), UsageError>,
) -> ProviderUsage {
    use super::types::unavailable;

    let client = match shared_client() {
        Ok(c) => c,
        Err(e) => return unavailable(provider, e),
    };

    match build_request(&client).send() {
        Ok(r) if r.status() == reqwest::StatusCode::TOO_MANY_REQUESTS => unavailable(
            provider,
            "Rate limited — usage data temporarily unavailable".to_string(),
        ),
        Ok(r) if !r.status().is_success() => {
            let code = r.status().as_u16();
            unavailable(
                provider,
                format!("API error {}: {}", code, r.text().unwrap_or_default()),
            )
        }
        Ok(r) => match parse(&r.text().unwrap_or_default()) {
            Ok((windows, detail)) => ProviderUsage {
                provider: provider.to_string(),
                logged_in: true,
                windows,
                balance: None,
                detail,
                error: None,
            },
            Err(e) => unavailable(provider, format!("Failed to parse response: {}", e)),
        },
        Err(e) => unavailable(provider, format!("Request failed: {}", e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preferences::BillingMode;

    fn account(id: &str, api_key: Option<&str>) -> ProviderAccount {
        ProviderAccount {
            id: id.to_string(),
            name: id.to_string(),
            enabled: true,
            billing_mode: BillingMode::PayAsYouGo,
            claude_compatible: true,
            api_key: api_key.map(str::to_string),
        }
    }

    #[test]
    fn api_key_rejects_missing_and_empty_keys() {
        assert_eq!(api_key_for(&[], "keyed-test"), None);
        assert_eq!(api_key_for(&[account("keyed-test", None)], "keyed-test"), None);
        assert_eq!(
            api_key_for(&[account("keyed-test", Some(""))], "keyed-test"),
            None
        );
        assert_eq!(
            api_key_for(&[account("keyed-test", Some("k"))], "keyed-test"),
            Some("k")
        );
        // Wrong provider id never leaks a key across providers.
        assert_eq!(
            api_key_for(&[account("other", Some("k"))], "keyed-test"),
            None
        );
    }
}
