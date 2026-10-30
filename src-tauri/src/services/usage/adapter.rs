//! The `UsageAdapter` seam (issue #1657, deepened in #1745).
//!
//! After the split the `usage` module keeps wire types ([`super::types`]) +
//! cache ([`super::cache`]) + the outcome taxonomy ([`super::outcome`]) only.
//! One trait sits between the catalog table and per-provider adapters:
//!
//! ```text
//! usage module (types + outcome + cache only, deep: small interface, shared taxonomy)
//!              │ seam: UsageAdapter { id(), fetch(accounts) -> UsageOutcome }
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
//!   the adapter's [`super::outcome::ResponsePolicy`], eliminating the
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
//! As of issue #1758 every adapter's `fetch` returns `UsageOutcome`
//! directly; the `From<ProviderUsage>` migration shim is removed, so no
//! adapter builds `ProviderUsage` literals anymore — the
//! [`super::outcome::UsageOutcome::into_usage`] projection (invoked by the
//! catalog) is the sole mint site of the wire shape.

use super::outcome::{ResponsePolicy, UsageEnrichmentPatch, UsageOutcome, UsageReading};
use super::types::{UsageError, UsagePage};
use crate::preferences::ProviderAccount;
use crate::services::http_client::blocking_client_builder;
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
    /// Provider-owned page for this meter's billing identity, when available.
    fn usage_page(&self) -> Option<UsagePage> {
        None
    }
    fn native_harness(&self) -> Option<&'static str> {
        None
    }

    /// Multiple harness majors may share one credential and billing identity.
    fn additional_native_harnesses(&self) -> &'static [&'static str] {
        &[]
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
static SHARED_CLIENT: std::sync::OnceLock<Result<Client, String>> = std::sync::OnceLock::new();

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
            blocking_client_builder()
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

/// Generic loopback HTTP fixture for adapter integration tests
/// (moved from `services::usage::tests` in ARCH-1 so provider test
/// modules own their fixtures without depending on the god-module).
/// Stands up a `tiny_http` server on `127.0.0.1:0` that dispatches
/// on `req.url()`; the re-export at `crate::services::usage::spawn_loopback`
/// keeps existing users (`freebuff_usage`, `cursor`, `anthropic`) working.
#[cfg(test)]
pub(crate) fn spawn_loopback<F>(max_requests: usize, handler: F) -> u16
where
    F: Fn(tiny_http::Request) + Send + 'static,
{
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread;

    let server = tiny_http::Server::http("127.0.0.1:0").expect("bind loopback");
    let port = match server.server_addr() {
        tiny_http::ListenAddr::IP(std::net::SocketAddr::V4(v4)) => v4.port(),
        other => panic!("expected a v4 loopback listener, got {other:?}"),
    };
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_thread = counter.clone();
    thread::spawn(move || {
        for request in server.incoming_requests() {
            handler(request);
            if counter_thread.fetch_add(1, Ordering::SeqCst) + 1 >= max_requests {
                return;
            }
        }
    });
    port
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
/// as logged-in-but-unavailable. `parse` maps a 2xx body to a
/// [`UsageReading`] — windowed quota, an optional prepaid balance, meters and
/// a detail line.
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
/// [`ResponsePolicy`] so each provider gets the right variant.
///
/// **Issue #2528 step 1**: the parse step widened from `(windows, detail)`
/// to a [`UsageReading`], so a balance-style reading rides the shared driver
/// instead of forcing a hand-rolled ladder, and the driver's failure wording
/// moved into an explicit [`ResponsePolicy`] so per-provider remediation copy
/// is a parameter rather than a forked implementation.
///
/// The provider name is set by the catalog projection (see
/// [`UsageOutcome::into_usage`]); this driver is intentionally
/// provider-agnostic so adapters can't accidentally leak provider-id
/// strings into the error variants.
pub(crate) fn fetch_usage<F1, F2>(
    policy: ResponsePolicy,
    build_request: F1,
    parse: F2,
) -> UsageOutcome
where
    F1: FnOnce(&Client) -> RequestBuilder,
    F2: FnOnce(&str) -> Result<UsageReading, UsageError>,
{
    let client = match shared_client() {
        Ok(c) => c,
        Err(e) => return UsageOutcome::Unavailable { reason: e },
    };
    match fetch_reading(&client, policy, build_request, parse) {
        Ok(reading) => reading.into_outcome(),
        Err(outcome) => outcome,
    }
}

/// Drives the shared request → status-check → parse flow plus one
/// best-effort enrichment read.
///
/// The enrichment is a second, independent read that *adds* to the primary
/// one (Command Code's subscription lookup supplies the monthly meter the
/// credits endpoint does not carry). It is best-effort: a failed enrichment
/// leaves the primary read's reading as it was, which
/// `outcome::UsageOutcome::Enriched` and the projection test beside it pin.
///
/// The returned outcome is [`UsageOutcome::Enriched`], whose wire projection
/// is the primary reading with the enrichment's contribution merged in —
/// see [`UsageEnrichmentPatch`] for the merge rules.
pub(crate) fn fetch_usage_with_enrichment<F1, F2, F3>(
    policy: ResponsePolicy,
    build_request: F1,
    parse: F2,
    enrich: F3,
) -> UsageOutcome
where
    F1: FnOnce(&Client) -> RequestBuilder,
    F2: FnOnce(&str) -> Result<UsageReading, UsageError>,
    F3: FnOnce(&Client) -> Result<Option<UsageEnrichmentPatch>, UsageError>,
{
    let client = match shared_client() {
        Ok(c) => c,
        Err(e) => return UsageOutcome::Unavailable { reason: e },
    };
    let mut reading = match fetch_reading(&client, policy, build_request, parse) {
        Ok(reading) => reading,
        Err(outcome) => return outcome,
    };
    if let Ok(Some(patch)) = enrich(&client) {
        // A successful enrichment appends its windows and replaces the
        // reading's balance and detail line — see [`UsageEnrichmentPatch`].
        reading.windows.extend(patch.windows);
        reading.balance = patch.balance;
        reading.detail = patch.detail;
    }
    // Otherwise the enrichment added no contribution, or it failed. Either
    // way the primary read's reading stands as it was. The provider's own closure
    // reports *why* its enrichment failed; the driver only records that the
    // second read was attempted.
    UsageOutcome::Enriched {
        reading: Box::new(reading.into_outcome()),
    }
}

/// The shared status ladder: one request, one status check, one parse.
///
/// Returns the parsed reading on success, or the outcome the failure maps to.
/// Split out of [`fetch_usage`] so the enriched driver runs the same
/// ladder before adding its second read.
fn fetch_reading<F1, F2>(
    client: &Client,
    policy: ResponsePolicy,
    build_request: F1,
    parse: F2,
) -> Result<UsageReading, UsageOutcome>
where
    F1: FnOnce(&Client) -> RequestBuilder,
    F2: FnOnce(&str) -> Result<UsageReading, UsageError>,
{
    // Production request → response → parse. `shared_client` (above) may
    // be a test override installed by [`with_client_override`], letting
    // the timeout test drive the production path with a 1s client.
    let request = build_request(client);
    let response = match request.send() {
        Ok(r) => r,
        Err(e) => {
            return Err(UsageOutcome::Unavailable {
                reason: format!("Request failed: {}", e),
            })
        }
    };

    let status = response.status();
    if matches!(status.as_u16(), 401 | 403) {
        let body = response.text().unwrap_or_default();
        let hint = match policy.rejected_hint {
            Some(hint) => hint.to_string(),
            None => format!("API error {}: {}", status.as_u16(), body),
        };
        return Err(UsageOutcome::Rejected { hint });
    }
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(UsageOutcome::RateLimited {
            reason: "Rate limited — usage data temporarily unavailable".to_string(),
        });
    }
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        return Err(UsageOutcome::Unavailable {
            reason: format!(
                "API error {}: {}",
                status.as_u16(),
                policy.error_body.render(&body)
            ),
        });
    }

    let body = match response.text() {
        Ok(body) => body,
        Err(error) => match policy.body_read_error {
            Some(copy) => {
                return Err(UsageOutcome::Unavailable {
                    reason: format!("{copy}: {error}"),
                })
            }
            // The driver's original behaviour: an unreadable body reads as an
            // empty body, so the parse step is the one that reports the
            // failure.
            None => String::new(),
        },
    };
    parse(&body).map_err(|error| UsageOutcome::Unavailable {
        reason: format!("Failed to parse response: {}", error),
    })
}

#[cfg(test)]
#[path = "adapter_tests.rs"]
mod tests;
