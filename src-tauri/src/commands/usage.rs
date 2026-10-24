//! Tauri commands for provider usage fetching.

use crate::preferences::{self, HarnessProfile, ProviderAccount};
use crate::services::usage::last_known::{LastKnown, UsageLastKnownCache};
use crate::services::usage::outcome::UsageOutcome;
use crate::services::usage::{self, ProviderMeters, ProviderUsage};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tauri::command;

/// Whether a harness backing `harness_id` was detected on the host. Startup
/// detection appends one [`HarnessProfile`] per installed tool with its legacy
/// provider id in the `harness` field, so presence there *is* "installed"
/// (issue #536).
fn harness_detected(harness_id: &str, profiles: &[HarnessProfile]) -> bool {
    profiles.iter().any(|p| p.harness == harness_id)
}

/// Whether a provider's card belongs on the Providers page for THIS host (issue
/// #574, detection-gated). A **native** provider appears only when its harness is
/// installed — it self-authenticates, so no key is needed and an uninstalled
/// harness is never shown (e.g. no Codex card on a box without Codex). A **keyed**
/// provider (first-class MiniMax/Kimi/OpenRouter or a Generic custom endpoint)
/// always has a card so its credential editor and enable toggle stay reachable.
/// The Kimi Code CLI Agent Harness itself doesn't appear as a provider card —
/// it's a Harness, registered in `HarnessProfile` / `Provider::Kimi` /
/// `KIMI` adapter, and surfaces through the Spawn Menu's native-row builder
/// rather than this account-driven visibility gate.
///
/// This is *card* visibility, deliberately independent of `enabled`: a user who
/// disables a provider must keep its card (showing "Disabled") so they can turn
/// it back on — `enabled` gates polling (see [`poll_ids`]), not the card. Pure
/// (no disk/network) so it's the unit-test seam.
fn account_visible(account: &ProviderAccount, profiles: &[HarnessProfile]) -> bool {
    match usage::catalog::native_harness(&account.id) {
        Some(harness) => harness_detected(harness, profiles),
        None => true,
    }
}

/// Whether Buildmesh ships a usage fetcher for this provider id. `false` for a
/// **Generic Model Provider** — surfaces as "usage not tracked".
fn usage_tracked(account_id: &str) -> bool {
    usage::catalog::contains(account_id)
}

/// The provider ids to actually poll: enabled, visible accounts whose usage
/// Buildmesh can fetch, preserving account order. Self-contained (gates on the
/// enable toggle AND detection AND a fetcher) so a caller can't poll a disabled,
/// hidden, or untracked provider; pure so it's unit-tested without a host scan or
/// the network.
fn poll_ids(accounts: &[ProviderAccount], profiles: &[HarnessProfile]) -> Vec<String> {
    accounts
        .iter()
        .filter(|a| a.enabled && account_visible(a, profiles) && usage_tracked(&a.id))
        .map(|a| a.id.clone())
        .collect()
}

/// The keyed first-class provider ids that have a non-empty resolved API key.
/// Read once per `get_provider_meters` call and threaded into
/// [`assemble_meters`] so the gate can distinguish "credential configured"
/// from "credential rejected" while keeping assembly pure for unit tests.
fn configured_keyed_providers(accounts: &[ProviderAccount]) -> HashSet<String> {
    usage::catalog::configured_keyed_provider_ids(accounts)
}

/// Build the Providers-page rows from the gated account set and a map of
/// already-fetched usages (keyed by provider id). Pure so the detection-gating +
/// usage-tracked derivation is unit-tested without touching the network: only
/// visible accounts appear (an uninstalled native harness is dropped entirely),
/// `usage` is populated for tracked providers with confirmed credentials, and a
/// Generic provider gets `usage_tracked = false` with no usage.
///
/// **No-credential gate.** A row is dropped entirely when the provider is
/// enabled + tracked AND has no credential configured AND the fetcher
/// returned `usage.logged_in == false`. This honours the user contract for
/// the glanceable Probe surface: "If we haven't added credentials for a
/// provider I shouldn't see the usage meter for that provider at all."
///
/// Why condition on the configured-key set rather than just
/// `usage.logged_in`? Because keyed fetchers (Kimi, OpenRouter — see
/// `services::usage::{kimi_usage,openrouter_usage}`) still return
/// `logged_in = false` on HTTP 401/403 — i.e. when the user's stored key
/// has been revoked, expired, or mistyped. That 401-vs-no-key distinction
/// has NOT yet moved into the seam (issue #1657 step 5 is an explicit
/// follow-up): until adapters report `logged_out` (no credential) vs
/// `unavailable` (credential present but fetch failed) precisely, the
/// account-level key-presence gate here stays necessary. Conflating "no
/// credential" with "credential is bad" would silently drop the row for
/// those users with no in-tab signal to re-enter their key. By gating on
/// the *account-level* key presence (the `configured_keys` parameter),
/// the row stays visible when the key exists but the API rejected it —
/// `<UsagePanel>` renders the existing "Invalid API key" copy
/// (UsageRender.tsx:115) and the user has a path back to Settings.
/// `assemble_meters` itself has no per-provider branches.
///
/// Two cases intentionally bypass this gate:
///
/// 1. **Disabled provider.** A user who explicitly disabled a provider must
///    keep the card so they can re-enable. We include with `usage = None`
///    so any caller (e.g. Settings-side AccountCard) can render its own
///    disabled affordance. The Probe Panel's UsageTab filters disabled
///    rows client-side, so the `usage: None` payload is unused there.
/// 2. **Generic untracked provider.** A custom Claude-compatible provider
///    has no usage fetcher by definition; the "Usage not tracked" card is
///    informative UX telling the user why. Included with `usage_tracked:
///    false` so the UI can render that copy.
///
/// The `?` operator on `usages.get(&a.id)` is a defensive guard: every id
/// returned by [`poll_ids`] is fanned out via `cached_or_fetch` and
/// inserted into the map, but a future refactor that decouples fetch
/// dispatch from map insertion would otherwise re-introduce a silent
/// drop. We drop the row in that case (matches the "polled but no data
/// arrived" semantics of a transient fetch failure that didn't produce
/// even a `logged_out` envelope).
///
/// **Last-known fallback (ADR-0037).** When the predicate says drop (no usable
/// credential right now), the row is instead served from `previous` — the durable store of
/// the last reading each provider reported, read before this round's fan-out —
/// with `cached_at` stamped so the UI can label it. Transient failures
/// (`RateLimited`/`Unavailable`) fall back the same way when a reading is
/// remembered: a slightly outdated meter beats a bare error, and the stamp
/// keeps it labelled as last known. Deliberately *not* applied to a
/// rejected-but-configured key (its re-entry affordance must stay).
/// An empty `previous` (fresh install, or nothing within the 7-day TTL) leaves
/// behaviour exactly as it was.
fn assemble_meters(
    accounts: &[ProviderAccount],
    profiles: &[HarnessProfile],
    usages: &HashMap<String, ProviderUsage>,
    outcomes: &HashMap<String, UsageOutcome>,
    configured_keys: &HashSet<String>,
    previous: &HashMap<String, LastKnown>,
) -> Vec<ProviderMeters> {
    accounts
        .iter()
        .filter(|a| account_visible(a, profiles))
        .filter_map(|a| {
            let tracked = usage_tracked(&a.id);
            // Disabled: keep the card so the user can re-enable.
            if !a.enabled {
                return Some(ProviderMeters {
                    provider: a.id.clone(),
                    usage_page: None,
                    usage_tracked: tracked,
                    usage: None,
                    cached_at: None,
                });
            }
            // Generic untracked: keep the "Usage not tracked" card.
            if !tracked {
                return Some(ProviderMeters {
                    provider: a.id.clone(),
                    usage_page: None,
                    usage_tracked: false,
                    usage: None,
                    cached_at: None,
                });
            }
            // Enabled + tracked: drop iff no credential is configured AND
            // the fetcher could not authenticate. The configured_keys set
            // lets us distinguish "no key" (drop) from "key present but
            // API rejected it" (keep — see the docstring for the user-
            // visible reason). For native self-auth providers, the
            // fetcher's `logged_in = false` already correlates with "no
            // credential on disk" with acceptable accuracy; we still
            // honour configured_keys for any future keyed native flows.
            //
            // Issue #1745: the keep/drop decision is keyed on the
            // outcome taxonomy, not on the wire triple. `NoCredential`
            // always drops (even if a key is configured — that is an
            // adapter bug, but the gate stays safe). `Rejected` drops
            // when no key is configured, keeps when configured (the
            // "Invalid API key" affordance). Every other variant keeps
            // the row: transient failures serve the remembered reading
            // when one exists (transient arm below), otherwise the live
            // error; degraded and managed-externally rows show their hint.
            let outcome = outcomes.get(&a.id);
            let usage = usages.get(&a.id);
            if outcome.is_some_and(|o| o.keep(configured_keys, &a.id)) {
                // Transient failures prefer a remembered reading when one
                // exists: a slightly outdated meter beats a bare error (for example
                // Muse reporting no subscription usage). With nothing
                // remembered the live error stays, exactly as before.
                let transient = outcome.is_some_and(|o| o.is_transient());
                if transient {
                    if let Some(last_known) = previous.get(&a.id) {
                        return Some(ProviderMeters {
                            provider: a.id.clone(),
                            usage_page: None,
                            usage_tracked: tracked,
                            usage: Some(last_known.usage.clone()),
                            cached_at: Some(last_known.cached_at),
                        });
                    }
                }
                return Some(ProviderMeters {
                    provider: a.id.clone(),
                    usage_page: None,
                    usage_tracked: tracked,
                    usage: Some(usage?.clone()),
                    cached_at: None,
                });
            }
            // ADR-0037: the row would be hidden this round — no usable
            // credential, e.g. Antigravity/Grok/Muse not logged into yet today.
            // Restore the last reading this provider *did* report, stamped with
            // when it was fetched so the UI labels it as last known. With nothing
            // remembered the row is dropped exactly as it was before.
            let last_known = previous.get(&a.id)?;
            Some(ProviderMeters {
                provider: a.id.clone(),
                usage_page: None,
                usage_tracked: tracked,
                usage: Some(last_known.usage.clone()),
                cached_at: Some(last_known.cached_at),
            })
        })
        .map(|mut meter| {
            meter.usage_page = usage::catalog::usage_page(&meter.provider, meter.usage.as_ref());
            meter
        })
        .collect()
}

/// Returns the detection-gated Providers-page rows: one [`ProviderMeters`] per
/// provider relevant to this host (issue #574). Native subscription meters appear
/// only for installed harnesses; keyed providers only when enabled; Generic
/// providers carry `usage_tracked = false`. Reuses the `ProviderUsage` wire shape.
///
/// Rows whose fetch yields no usable credential, or a transient failure while
/// a reading is remembered, are served from the durable last-known store
/// instead of being dropped or erroring (ADR-0037); see [`assemble_meters`].
#[command]
pub async fn get_provider_meters(
    force_refresh: bool,
    last_known: tauri::State<'_, UsageLastKnownCache>,
) -> Result<Vec<ProviderMeters>, String> {
    // `Clone` is an Arc bump — required to move the cache onto the blocking pool
    // (same reason `GhAuthCache` is `Clone`).
    let last_known = last_known.inner().clone();
    let snapshot_cache = last_known.clone();
    let (profiles, accounts, ids, configured_keys, previous) =
        crate::commands::run_blocking("get_provider_meters_ids", move || {
            let profiles = preferences::harness_profiles();
            let accounts = Arc::new(preferences::provider_accounts());
            let ids = poll_ids(&accounts, &profiles);
            // Derive credential presence from the same effective account
            // snapshot the fetch workers receive. No worker reloads preferences.
            let configured_keys = configured_keyed_providers(&accounts);
            // Read the durable last-known readings BEFORE the fan-out. This is
            // the cold-start read: it makes a restart work (yesterday's reading
            // is available before today's first fetch), and it keeps this
            // round's successful fetches out of this round's own fallback.
            let previous = snapshot_cache.snapshot();
            Ok((profiles, accounts, ids, configured_keys, previous))
        })
        .await?;

    // Each fetch is a blocking HTTP round-trip to a different vendor; running
    // them serially made the panel wait for the sum of all of them. Fan out on
    // blocking threads and collect into a map keyed by provider id.
    //
    // Issue #1745: each worker also returns the raw `UsageOutcome` so the
    // gate can decide keep/drop on the outcome taxonomy rather than on the
    // wire triple. The catalog caches the projected `ProviderUsage` (the
    // wire triple is unchanged for the IPC); the outcome lives only in
    // this command closure.
    let handles: Vec<_> = ids
        .into_iter()
        .map(|id| {
            let accounts = Arc::clone(&accounts);
            tauri::async_runtime::spawn_blocking(move || {
                let (outcome, usage) =
                    usage::catalog::cached_outcome_and_usage(&id, force_refresh, accounts.as_ref())
                        .ok_or_else(|| format!("usage catalog lost registered provider: {id}"))?;
                Ok::<_, String>((id, outcome, usage))
            })
        })
        .collect();

    let mut usages: HashMap<String, ProviderUsage> = HashMap::new();
    let mut outcomes: HashMap<String, UsageOutcome> = HashMap::new();
    let mut readings: Vec<(String, ProviderUsage)> = Vec::new();
    for handle in handles {
        let result = handle
            .await
            .map_err(|e| format!("usage fetch task failed: {}", e))?;
        let (id, outcome, usage) = result?;
        if outcome.is_reading() {
            readings.push((id.clone(), usage.clone()));
        }
        outcomes.insert(id.clone(), outcome);
        usages.insert(id, usage);
    }

    // Remember what we actually learned, for a future round that cannot fetch.
    // One blocking hop for the whole batch (disk I/O must stay off the async
    // worker pool — see the *Command Threading* convention) and one file write
    // per refresh rather than one per provider.
    crate::commands::run_blocking("usage_last_known_record", move || {
        last_known.record_many(&readings);
        Ok(())
    })
    .await?;

    Ok(assemble_meters(
        accounts.as_ref(),
        &profiles,
        &usages,
        &outcomes,
        &configured_keys,
        &previous,
    ))
}

#[command]
pub async fn set_minimax_api_key(key: Option<String>) -> Result<(), String> {
    crate::commands::run_blocking("set_minimax_api_key", move || {
        let mut prefs = preferences::load()?;
        prefs.minimax_api_key = key;
        preferences::save(prefs)?;
        usage::invalidate_cache();
        Ok(())
    })
    .await
}

#[cfg(test)]
#[path = "usage_tests.rs"]
mod tests;
