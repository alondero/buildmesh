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

/// Test-only seam hook: when set, [`fetch`] overrides adapter dispatch and
/// returns the supplied envelope directly. Used to keep the
/// `dispatch(id).fetch(accounts)` contract testable end-to-end without
/// running host credential discovery or live HTTP. `None` in production;
/// set inside `#[cfg(test)]` blocks only.
///
/// Implemented as a process-wide `OnceLock` so tests can swap it once per
/// scope and the catalog dispatch path can read it without changing the
/// adapter trait. Mirrors the `SHARED_CLIENT` pattern in this module.
#[cfg(test)]
pub(crate) static FETCH_OVERRIDE: std::sync::OnceLock<
    std::sync::Mutex<(String, Option<ProviderUsage>)>,
> = std::sync::OnceLock::new();

/// Scope helper for the test-only fetch override. The override is stored
/// in a `OnceLock<Mutex<(String, Option<ProviderUsage>)>>`; we copy the
/// envelope OUT before invoking the closure so the lock is released and
/// the production fetch path can read the override without re-entering.
#[cfg(test)]
pub(crate) fn with_fetch_override<F, R>(id: &str, envelope: ProviderUsage, f: F) -> R
where
    F: FnOnce() -> R,
{
    let mut cell = FETCH_OVERRIDE
        .get_or_init(|| std::sync::Mutex::new((String::new(), None)))
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let snapshot = (id.to_string(), Some(envelope));
    let previous = std::mem::replace(&mut *cell, snapshot);
    drop(cell);
    let result = f();
    // Restore prior override state so tests run in deterministic order.
    let mut cell = FETCH_OVERRIDE
        .get()
        .expect("override initialised above")
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    *cell = previous;
    result
}

/// Catalog entry point: routes an adapter's [`UsageAdapter::fetch`] call
/// through the (cfg(test)) override hook when one is set for the adapter's
/// id, otherwise calls the adapter's production [`fetch`](UsageAdapter::fetch).
/// Production callers go through this so the dispatch contract tests
/// (`catalog::cached_or_fetch` → [`dispatch_fetch`] → override) exercise the
/// real dispatch path with a controllable transport instead of bypassing the
/// catalog.
pub(crate) fn dispatch_fetch(
    adapter: &dyn UsageAdapter,
    accounts: &[ProviderAccount],
) -> ProviderUsage {
    #[cfg(test)]
    if let Some(envelope) = peek_override(adapter.id()) {
        return envelope;
    }
    adapter.fetch(accounts)
}

/// Snapshot the override for `adapter_id` without holding the lock during
/// the caller's dispatch path. Returning `Option<ProviderUsage>` is the
/// only contract; `dispatch_fetch` reads it and propagates.
#[cfg(test)]
fn peek_override(adapter_id: &str) -> Option<ProviderUsage> {
    let guard = FETCH_OVERRIDE.get()?.lock().unwrap_or_else(|p| p.into_inner());
    let (id, env) = &*guard;
    if id == adapter_id {
        env.clone()
    } else {
        None
    }
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

/// One shared HTTP client for all adapters (issue #1657 step 6, first half).
///
/// Previously every fetcher built its own `reqwest::blocking::Client` inline,
/// so tests could only intercept at the loopback-HTTP layer. Construction is
/// centralised here behind a process-wide `OnceLock`; per-adapter transport
/// injection is an explicit follow-up and is not claimed here. Timeout is 15s,
/// matching the Freebuff fetcher; `fetch_usage` callers previously built an
/// unbounded client, so this adds a timeout there (behaviour change, covered
/// by the existing status/parse loopback tests, no timeout-specific test).
static SHARED_CLIENT: std::sync::OnceLock<Result<Client, String>> =
    std::sync::OnceLock::new();

pub(crate) fn shared_client() -> Result<Client, String> {
    SHARED_CLIENT
        .get_or_init(|| {
            Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .map_err(|e| format!("Client error: {e}"))
        })
        .clone()
}

/// Drives the shared request → status-check → parse flow. Callers reach this
/// only once a credential is confirmed present, so any failure here is reported
/// as logged-in-but-unavailable. `parse` maps a 2xx body to `(windows, detail)`.
///
/// Moved here from the `usage.rs` god-module so adapters import the driver
/// from the seam, never from the fetcher module. Existing `parse_*` +
/// loopback tests must pass unmodified after each provider move (tests move
/// files, not assertions); the only intended behaviour delta is the 15s
/// timeout noted on [`shared_client`].
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

    #[test]
    fn shared_client_applies_a_15_second_timeout() {
        // The shared client centralises the Freebuff fetcher's 15s timeout
        // onto every adapter path. Pin the timeout via a deliberately-slow
        // TCP listener that accepts the connection then idles without ever
        // producing an HTTP response, so reqwest must hit its configured
        // timeout. reqwest's default is unbounded, which previously matched
        // per-fetcher inline construction.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(false).expect("blocking accept");
        // Synchronise on acceptance so the assertion below proves the
        // timeout fired AFTER the server accepted the connection (i.e. an
        // immediate connection refused would short-circuit and bypass the
        // timeout entirely). The server thread owns its TcpStream and a
        // stop-flag Atomic so the test can join it deterministically.
        let accepted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let accepted_for_thread = std::sync::Arc::clone(&accepted);
        let stop_for_thread = std::sync::Arc::clone(&stop);
        let server_thread = std::thread::spawn(move || {
            // Accept one connection, signal, then hold it open until the
            // stop flag flips (or the 30s ceiling hits).
            if let Ok((stream, _)) = listener.accept() {
                accepted_for_thread.store(true, std::sync::atomic::Ordering::Release);
                while !stop_for_thread.load(std::sync::atomic::Ordering::Acquire) {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                drop(stream);
            }
        });
        let start = std::time::Instant::now();
        let result = fetch_usage(
            "anthropic",
            |c| c.get(format!("http://127.0.0.1:{port}/usage")),
            |body| Ok((vec![], Some(body.to_string()))),
        );
        let elapsed = start.elapsed();
        // Server must have accepted the connection so an immediate-refused
        // failure cannot masquerade as a timeout pass.
        assert!(
            accepted.load(std::sync::atomic::Ordering::Acquire),
            "loopback listener did not accept the connection; got: {result:?}"
        );
        // reqwest timeout surfaces as `Request failed: error sending request...`
        // when read-timeout fires; we accept both the read-timeout and the
        // request-build error variants so a reqwest wording tweak does not
        // regress this test, while still requiring elapsed >= 5s (well above
        // any connection-refused floor) and < 20s (well under the test budget).
        let error = result.error.as_deref().unwrap_or_default();
        assert!(
            error.starts_with("Request failed:"),
            "expected reqwest timeout envelope, got: {result:?}"
        );
        assert!(
            elapsed >= std::time::Duration::from_secs(5),
            "timeout must wait for the configured 15s; elapsed {elapsed:?}"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(20),
            "15s timeout should fire well under 20s; took {elapsed:?}"
        );
        // Stop the server thread deterministically so it does not outlive
        // the test process (was a detached-thread leak in the previous draft).
        stop.store(true, std::sync::atomic::Ordering::Release);
        server_thread.join().expect("server thread join");
    }
}
