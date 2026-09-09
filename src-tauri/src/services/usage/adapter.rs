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
/// so tests could only intercept at the loopback-HTTP layer. Construction is
/// centralised here behind a process-wide `OnceLock`; the test-only
/// [`with_client_override`] seam lets the timeout test drive the production
/// [`fetch_usage`] path with a 1-second client so the assertion is a
/// narrow band tied to the configured value (1s) instead of a 5..20s band
/// that admits a regression. Timeout is 15s, matching the Freebuff fetcher;
/// `fetch_usage` callers previously built an unbounded client, so this adds
/// a timeout there (behaviour change covered by
/// [`shared_client_applies_a_configured_request_timeout`]).
static SHARED_CLIENT: std::sync::OnceLock<Result<Client, String>> =
    std::sync::OnceLock::new();

pub(crate) fn shared_client() -> Result<Client, String> {
    // Test-only override (set by `with_client_override`); when present,
    // use it INSTEAD of the production 15s client so the timeout test
    // can assert the configured timeout deterministically without
    // duplicating fetch_usage's request/status/parse algorithm.
    #[cfg(test)]
    if let Some(client) = crate::services::usage::adapter::current_client_override() {
        return Ok(client);
    }
    SHARED_CLIENT
        .get_or_init(|| {
            Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .map_err(|e| format!("Client error: {e}"))
        })
        .clone()
}

// Test-only client override: when set on the current thread,
// `shared_client` returns it instead of the production 15s client.
// Thread-local (not process-wide) so parallel `cargo test` workers stay
// isolated: one worker installing a 1s timeout client cannot poison
// another worker's production fetch path. Scoped via
// `with_client_override` (panic-safe RAII). The timeout test uses this
// to inject a 1s client so the timeout assertion is a narrow band tied
// to the configured value.
#[cfg(test)]
thread_local! {
    static CLIENT_OVERRIDE: std::cell::RefCell<Option<Client>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn current_client_override() -> Option<Client> {
    CLIENT_OVERRIDE.with(|cell| cell.borrow().clone())
}

/// RAII guard that restores the previous `CLIENT_OVERRIDE` value on Drop
/// (including during unwinding). Constructed only via
/// [`with_client_override`].
#[cfg(test)]
struct ClientOverrideGuard {
    previous: Option<Client>,
}

#[cfg(test)]
impl Drop for ClientOverrideGuard {
    fn drop(&mut self) {
        CLIENT_OVERRIDE.with(|cell| {
            *cell.borrow_mut() = self.previous.take();
        });
    }
}

/// Scope helper: install `client` as the [`shared_client`] override for
/// the duration of `f`. Used by the timeout test so it can drive the
/// production [`fetch_usage`] path with an HTTP client that has a 1s
/// timeout (rather than the production 15s). Returns `f()`'s result.
#[cfg(test)]
pub(crate) fn with_client_override<F, R>(client: Client, f: F) -> R
where
    F: FnOnce() -> R,
{
    let guard = ClientOverrideGuard::install(client);
    let result = f();
    drop(guard);
    result
}

#[cfg(test)]
impl ClientOverrideGuard {
    fn install(client: Client) -> Self {
        let previous = CLIENT_OVERRIDE.with(|cell| cell.borrow_mut().replace(client));
        Self { previous }
    }
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
pub(crate) fn fetch_usage<F1, F2>(
    provider: &str,
    build_request: F1,
    parse: F2,
) -> ProviderUsage
where
    F1: FnOnce(&Client) -> RequestBuilder,
    F2: FnOnce(&str) -> Result<(Vec<UsageWindow>, Option<String>), UsageError>,
{
    use super::types::unavailable;

    let client = match shared_client() {
        Ok(c) => c,
        Err(e) => return unavailable(provider, e),
    };

    // Production request → response → parse. `shared_client` (above) may
    // be a test override installed by [`with_client_override`], letting
    // the timeout test drive the production path with a 1s client.
    let request = build_request(&client);
    match request.send() {
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
        // Verify the configured timeout on the actual production fetch
        // path: install a 1s client via `with_client_override` (the
        // shared_client seam), drive the production `fetch_usage`
        // against an idle loopback listener, and assert the timeout
        // envelope AND a narrow band tied to the configured value
        // (0.8s..1.8s). The previous draft reproduced the
        // request/status/parse algorithm in `fetch_usage_with`; this
        // test exercises the production code itself.
        //
        // Tying the upper bound to the configured value with a small
        // tolerance catches the regression class where the timeout
        // is reduced (say) to 5s without anybody noticing because the
        // test still passes.
        const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
        let client = Client::builder()
            .timeout(TIMEOUT)
            .build()
            .expect("test client build");
        let (port, _server) = spawn_idle_loopback();
        let start = std::time::Instant::now();
        let result = with_client_override(client, || {
            fetch_usage(
                "anthropic",
                |c| c.get(format!("http://127.0.0.1:{port}/usage")),
                |body| Ok((vec![], Some(body.to_string()))),
            )
        });
        let elapsed = start.elapsed();
        let error = result.error.as_deref().unwrap_or_default();
        assert!(
            error.starts_with("Request failed:"),
            "expected reqwest timeout envelope, got: {result:?}"
        );
        // IdleServer Drop joins the worker thread on every exit path
        // (success or panic); the assertion below proves the timeout
        // fired AFTER the connection was accepted, not because the
        // listener was closed prematurely.
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
    /// thread and joins it so a panic in the test body still cleans up.
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
}
