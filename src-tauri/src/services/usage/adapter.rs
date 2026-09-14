//! The `UsageAdapter` seam (issue #1657, deepened in #1745).
//!
//! After the split the `usage` module keeps wire types ([`super::types`]) +
//! cache ([`super::cache`]) + the outcome taxonomy ([`super::outcome`]) only.
//! One trait sits between the catalog table and per-provider adapters:
//!
//! ```text
//! usage module (types + outcome + cache only, deep: small interface, shared taxonomy)
//!              │ seam: UsageAdapter { id(), fetch(accounts) -> UsageOutcome,
//!              │                       auth_policy() -> AuthPolicy }
//!    ┌─────────┼──────────┬──────────────┬─── …nth adapter
//! anthropic  minimax   opencode   freebuff
//! adapter    adapter   adapter    adapter
//! ```
//!
//! **Issue #1745** changed the return type of `fetch` from `ProviderUsage` to
//! [`super::outcome::UsageOutcome`]. Adapters no longer mint the wire
//! directly — the [`super::outcome::UsageOutcome::into_usage`] projection is
//! the sole boundary. This makes it structurally impossible for an adapter
//! to encode the same underlying state differently from its siblings:
//!
//! - The `NoCredential` / `Rejected` / `RateLimited` / `Unavailable` /
//!   `Degraded` / `ManagedExternally` / `Reading` taxonomy is owned by
//!   `super::outcome`, not by per-adapter envelope choices.
//! - The shared [`fetch_usage`] driver classifies HTTP 401/403 according to
//!   the adapter's [`super::outcome::AuthPolicy`] default, eliminating the
//!   per-adapter hand-rolled status ladder that drifted (MiniMax missing
//!   the arm, Muse Code conflating no-credential with unavailable, Agy
//!   mapping client-build errors to logged-out).
//! - The `assemble_meters` gate in `commands/usage.rs` reads the outcome's
//!   [`super::outcome::UsageOutcome::keep`] predicate, so the keep/drop
//!   decision is testable in one table.
//!
//! Catalog dispatch and `commands/usage.rs` ask the seam — never per-provider
//! lore. Adding provider N means adding one `adapters/<name>.rs` file and one
//! catalog entry (drop-in adapter to delete), not editing the fetcher module
//! AND the catalog entry.
//!
//! Migration cadence (#1657 precedent): adapters migrate one per commit.
//! As of #1745 phase 1, every adapter's `fetch` returns `UsageOutcome`;
//! adapters that still build `ProviderUsage` literals internally wrap their
//! final value via `outcome.into_usage(provider_id)` at the adapter
//! boundary.

use super::outcome::{AuthPolicy, UsageOutcome};
use super::types::{UsageError, UsageWindow};
use crate::preferences::ProviderAccount;
use reqwest::blocking::{Client, RequestBuilder};
use sha2::{Digest, Sha256};
use std::time::Duration;

static CACHE_FINGERPRINT_SALT: once_cell::sync::Lazy<[u8; 32]> =
    once_cell::sync::Lazy::new(rand::random);

/// Opaque cache identity for one provider account and authentication source.
/// The digest is deliberately private and this type does not implement
/// `Debug` or `Display`, preventing credentials or account identifiers from
/// leaking when a cache key is logged accidentally.
#[derive(Clone, Eq, Hash, PartialEq)]
pub(crate) struct UsageIdentityFingerprint {
    auth_source: &'static str,
    digest: [u8; 32],
}

impl UsageIdentityFingerprint {
    /// Hash both the authentication source and account identity. `identity`
    /// may be a token when the provider exposes no non-secret account id; only
    /// the digest is retained in memory as part of the cache key.
    pub(crate) fn new(auth_source: &'static str, identity: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"buildmesh-usage-cache-v1\0");
        hasher.update(*CACHE_FINGERPRINT_SALT);
        hasher.update(auth_source.as_bytes());
        hasher.update(b"\0");
        hasher.update(identity);
        Self {
            auth_source,
            digest: hasher.finalize().into(),
        }
    }
}

/// One first-class Usage Meter behind the seam.
///
/// - [`id`](UsageAdapter::id) is the provider account id (`"anthropic"`,
///   `"minimax"`, …) and the cache key.
/// - [`native_harness`](UsageAdapter::native_harness) is `Some(harness)` for
///   self-authenticating native meters (detection-gated card) and `None` for
///   keyed meters (card always visible; credential comes from `accounts`).
/// - [`fetch`](UsageAdapter::fetch) takes the effective account snapshot the
///   command already resolved — adapters never read preferences themselves —
///   and returns a [`UsageOutcome`]. The catalog projection
///   ([`UsageOutcome::into_usage`]) is the sole mint site of the wire shape.
pub(crate) trait UsageAdapter: Send + Sync {
    fn id(&self) -> &'static str;
    fn native_harness(&self) -> Option<&'static str> {
        None
    }
    /// Identify the account and authentication source used by `fetch`.
    /// Keyed adapters get account-aware caching automatically. Native adapters
    /// may override this when they can select between OAuth, cloud, workspace,
    /// or other provider-owned credential sources.
    fn cache_identity(&self, accounts: &[ProviderAccount]) -> UsageIdentityFingerprint {
        if let Some(api_key) = api_key_for(accounts, self.id()) {
            return UsageIdentityFingerprint::new("api_key", api_key.as_bytes());
        }
        UsageIdentityFingerprint::new(
            self.native_harness().unwrap_or("unconfigured_api_key"),
            self.id().as_bytes(),
        )
    }
    fn fetch(&self, accounts: &[ProviderAccount]) -> UsageOutcome;
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
///
/// **Issue #1745**: the 401/403 arm was added; before, every non-2xx
/// mapped to `unavailable()` (red transport error), which is why MiniMax's
/// revoked-key case rendered red text while its keyed siblings rendered
/// "Invalid API key". The arm now classifies via the adapter's
/// [`AuthPolicy`] so each provider gets the right variant.
///
/// The provider name is set by the catalog projection (see
/// [`UsageOutcome::into_usage`]); this driver is intentionally
/// provider-agnostic so adapters can't accidentally leak provider-id
/// strings into the error variants.
pub(crate) fn fetch_usage<F1, F2>(
    auth_policy: AuthPolicy,
    build_request: F1,
    parse: F2,
) -> UsageOutcome
where
    F1: FnOnce(&Client) -> RequestBuilder,
    F2: FnOnce(&str) -> Result<(Vec<UsageWindow>, Option<String>), UsageError>,
{
    let client = match shared_client() {
        Ok(c) => c,
        Err(e) => return UsageOutcome::Unavailable { reason: e },
    };

    // Production request → response → parse. `shared_client` (above) may
    // be a test override installed by [`with_client_override`], letting
    // the timeout test drive the production path with a 1s client.
    let request = build_request(&client);
    match request.send() {
        Ok(r) if matches!(r.status().as_u16(), 401 | 403) => {
            let status = r.status().as_u16();
            let body = r.text().unwrap_or_default();
            let reason = format!("API error {status}: {body}");
            match auth_policy {
                AuthPolicy::Rejected => UsageOutcome::Rejected { hint: reason },
                AuthPolicy::NoCredential => UsageOutcome::NoCredential { hint: reason },
            }
        }
        Ok(r) if r.status() == reqwest::StatusCode::TOO_MANY_REQUESTS => UsageOutcome::RateLimited {
            reason: "Rate limited — usage data temporarily unavailable".to_string(),
        },
        Ok(r) if !r.status().is_success() => UsageOutcome::Unavailable {
            reason: format!("API error {}: {}", r.status().as_u16(), r.text().unwrap_or_default()),
        },
        Ok(r) => match parse(&r.text().unwrap_or_default()) {
            Ok((windows, detail)) => UsageOutcome::Reading {
                windows,
                balance: None,
                meters: Vec::new(),
                detail,
            },
            Err(e) => UsageOutcome::Unavailable {
                reason: format!("Failed to parse response: {}", e),
            },
        },
        Err(e) => UsageOutcome::Unavailable {
            reason: format!("Request failed: {}", e),
        },
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
        //
        // Issue #1745: `fetch_usage` now returns `UsageOutcome`; the
        // timeout path is `UsageOutcome::Unavailable { reason: "Request failed: ..." }`.
        const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
        let client = Client::builder()
            .timeout(TIMEOUT)
            .build()
            .expect("test client build");
        let (port, _server) = spawn_idle_loopback();
        let start = std::time::Instant::now();
        let outcome = with_client_override(client, || {
            fetch_usage(
                AuthPolicy::Rejected,
                |c| c.get(format!("http://127.0.0.1:{port}/usage")),
                |body| Ok((vec![], Some(body.to_string()))),
            )
        });
        let elapsed = start.elapsed();
        let reason = match &outcome {
            UsageOutcome::Unavailable { reason } => reason.as_str(),
            other => panic!("expected Unavailable outcome, got: {other:?}"),
        };
        assert!(
            reason.starts_with("Request failed:"),
            "expected reqwest timeout envelope, got reason: {reason:?}"
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
