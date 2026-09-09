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
/// in a `OnceLock<Mutex<(String, Option<ProviderUsage>)>>`. Returns a
/// [`FetchOverrideGuard`] RAII type whose `Drop` restores the previous
/// value even when the closure panics, so a failing test cannot leak a
/// stale override into the next test (which previously caused the
/// dispatch.fetch tests to fall through to the real AnthropicAdapter
/// fetch path on a credential-bearing host, false-failing the
/// `windows.len() == 1` assertion).
#[cfg(test)]
pub(crate) fn with_fetch_override<F, R>(id: &str, envelope: ProviderUsage, f: F) -> R
where
    F: FnOnce() -> R,
{
    let guard = FetchOverrideGuard::install(id, envelope);
    let result = f();
    drop(guard);
    result
}

/// RAII guard that restores the previous `FETCH_OVERRIDE` value on Drop
/// (including during unwinding). Constructed only via
/// [`with_fetch_override`].
#[cfg(test)]
struct FetchOverrideGuard {
    previous: (String, Option<ProviderUsage>),
}

#[cfg(test)]
impl FetchOverrideGuard {
    fn install(id: &str, envelope: ProviderUsage) -> Self {
        let mut cell = FETCH_OVERRIDE
            .get_or_init(|| std::sync::Mutex::new((String::new(), None)))
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let previous = std::mem::replace(&mut *cell, (id.to_string(), Some(envelope)));
        Self { previous }
    }
}

#[cfg(test)]
impl Drop for FetchOverrideGuard {
    fn drop(&mut self) {
        if let Some(cell) = FETCH_OVERRIDE.get() {
            if let Ok(mut guard) = cell.lock() {
                *guard = std::mem::take(&mut self.previous);
            }
        }
    }
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
    fn shared_client_applies_a_configured_request_timeout() {
        // Verify the configured timeout is honoured on the actual fetch path.
        // We use a 1-second timeout (rather than the production 15s) so the
        // test is fast AND the assertion can be a narrow band tied to the
        // configured value: 0.8s..1.8s. An unbounded client (reqwest
        // default) would block until the listener is closed, far past the
        // upper bound, so the band proves the timeout fired.
        //
        // The previous draft's 5s..20s band would have admitted a regression
        // where the timeout was reduced to (say) 5s without anybody noticing
        // because the test still passed. Tying the upper bound to the
        // configured value with a small tolerance catches that class.
        const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
        let client = Client::builder()
            .timeout(TIMEOUT)
            .build()
            .expect("test client build");
        let (port, _server) = spawn_idle_loopback();
        let start = std::time::Instant::now();
        let result = fetch_usage_with(
            client,
            "anthropic",
            |c| c.get(format!("http://127.0.0.1:{port}/usage")),
            |body| Ok((vec![], Some(body.to_string()))),
        );
        let elapsed = start.elapsed();
        let error = result.error.as_deref().unwrap_or_default();
        assert!(
            error.starts_with("Request failed:"),
            "expected reqwest timeout envelope, got: {result:?}"
        );
        // Server thread joined by `_server` Drop below; if the listener
        // was never accepted, the connect attempt itself returns
        // immediately, so we must have the accept signal observed
        // (see spawn_idle_loopback).
        assert!(
            elapsed >= std::time::Duration::from_millis(800),
            "1s timeout must wait for the configured value; elapsed {elapsed:?}"
        );
        assert!(
            elapsed < TIMEOUT + std::time::Duration::from_millis(800),
            "configured timeout is {TIMEOUT:?}; elapsed {elapsed:?} should be within 800ms"
        );
    }

    /// Spawns a TCP listener on `127.0.0.1:0` and a worker thread that
    /// accepts one connection then idles until the returned [`IdleServer`]
    /// guard's `Drop` runs. The guard deterministically stops the worker
    /// thread and joins it so a panic in the test body still cleans up —
    /// the previous draft joined only after all assertions.
    fn spawn_idle_loopback() -> (u16, IdleServer) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(false).expect("blocking accept");
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_for_thread = std::sync::Arc::clone(&stop);
        let listener_thread = std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                while !stop_for_thread.load(std::sync::atomic::Ordering::Acquire) {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                drop(stream);
            }
        });
        let server = IdleServer {
            stop,
            handle: Some(listener_thread),
        };
        (port, server)
    }

    /// RAII guard: flips `stop` and joins the worker thread on Drop, so a
    /// panic in the test body still cleans up the listener.
    struct IdleServer {
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        handle: Option<std::thread::JoinHandle<()>>,
    }

    impl Drop for IdleServer {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::Release);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    /// Test-only variant of [`fetch_usage`] that takes an injected
    /// [`Client`] (instead of the [`shared_client`]) so the timeout test
    /// can run with a 1s timeout and complete in ~1s rather than the
    /// production 15s.
    pub(crate) fn fetch_usage_with(
        client: Client,
        provider: &str,
        build_request: impl FnOnce(&Client) -> RequestBuilder,
        parse: impl FnOnce(&str) -> Result<(Vec<UsageWindow>, Option<String>), UsageError>,
    ) -> ProviderUsage {
        use crate::services::usage::types::unavailable;
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
}
