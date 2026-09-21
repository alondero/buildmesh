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
                    usage_tracked: tracked,
                    usage: None,
                    cached_at: None,
                });
            }
            // Generic untracked: keep the "Usage not tracked" card.
            if !tracked {
                return Some(ProviderMeters {
                    provider: a.id.clone(),
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
                let transient = matches!(
                    outcome,
                    Some(UsageOutcome::RateLimited { .. } | UsageOutcome::Unavailable { .. })
                );
                if transient {
                    if let Some(last_known) = previous.get(&a.id) {
                        return Some(ProviderMeters {
                            provider: a.id.clone(),
                            usage_tracked: tracked,
                            usage: Some(last_known.usage.clone()),
                            cached_at: Some(last_known.cached_at),
                        });
                    }
                }
                return Some(ProviderMeters {
                    provider: a.id.clone(),
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
                usage_tracked: tracked,
                usage: Some(last_known.usage.clone()),
                cached_at: Some(last_known.cached_at),
            })
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
                let (outcome, usage) = usage::catalog::cached_outcome_and_usage(
                    &id,
                    force_refresh,
                    accounts.as_ref(),
                )
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
        if matches!(&outcome, UsageOutcome::Reading { .. }) {
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
mod tests {
    use super::*;
    use crate::preferences::BillingMode;

    fn account(id: &str, enabled: bool) -> ProviderAccount {
        ProviderAccount {
            id: id.to_string(),
            name: id.to_string(),
            enabled,
            billing_mode: BillingMode::Plan,
            claude_compatible: crate::preferences::is_claude_compatible_id(id),
            api_key: None,
        }
    }

    fn profile(id: &str, harness: &str) -> HarnessProfile {
        HarnessProfile {
            id: id.to_string(),
            name: id.to_string(),
            harness: harness.to_string(),
            runtime: None, wsl_distro: None, executable: None,
        }
    }

    fn usage(provider: &str) -> ProviderUsage {
        ProviderUsage {
            provider: provider.to_string(),
            logged_in: true,
            windows: Vec::new(),
            balance: None,
            meters: vec![],
            detail: None,
            error: None,
        }
    }

    /// Issue #1745: tests now build an `outcomes` map alongside `usages`.
    /// Each entry's variant must match what `fetch` would have returned;
    /// the helpers below produce the matching outcome for each envelope
    /// shape used in this module's tests.
    #[allow(dead_code)]
    fn reading_outcome(_provider: &str) -> UsageOutcome {
        UsageOutcome::Reading {
            windows: Vec::new(),
            balance: None,
            meters: Vec::new(),
            detail: None,
        }
    }

    fn no_credential_outcome(provider: &str) -> UsageOutcome {
        UsageOutcome::NoCredential {
            hint: format!("No API key configured for {provider}"),
        }
    }

    fn outcomes_from_usages(usages: &HashMap<String, ProviderUsage>) -> HashMap<String, UsageOutcome> {
        // For each cached usage, project the variant the gate expects. The
        // gate's keep() predicate is total on every variant, so a wrong
        // mapping here silently changes keep/drop; tests must use the
        // helpers above so the projection is explicit.
        let mut map = HashMap::new();
        for (id, usage) in usages {
            let outcome = if usage.logged_in {
                UsageOutcome::Reading {
                    windows: usage.windows.clone(),
                    balance: usage.balance.clone(),
                    meters: usage.meters.clone(),
                    detail: usage.detail.clone(),
                }
            } else {
                UsageOutcome::NoCredential {
                    hint: usage.error.clone().unwrap_or_default(),
                }
            };
            map.insert(id.clone(), outcome);
        }
        map
    }

    // ── Detection gating (issue #574) ───────────────────────────────────────

    #[test]
    fn native_provider_visible_only_when_its_harness_is_detected() {
        // Claude Code installed (harness profile backed by "anthropic").
        let claude = vec![profile("claude", "anthropic")];
        assert!(account_visible(&account("anthropic", true), &claude));
        // No harness profiles → the Anthropic subscription card is hidden (AC2's
        // inverse: it only shows when Claude Code is detected).
        assert!(!account_visible(&account("anthropic", true), &[]));
        // Codex card never shows on a box without Codex even though Claude is here.
        assert!(!account_visible(&account("codex", true), &claude));
    }

    #[test]
    fn native_provider_card_stays_visible_when_disabled_but_detected() {
        // Card visibility is detection-driven, not enable-driven: a user who
        // disables an installed provider keeps its card so they can re-enable it.
        // (`enabled` gates polling, exercised in the poll_ids tests.)
        let claude = vec![profile("claude", "anthropic")];
        assert!(account_visible(&account("anthropic", false), &claude));
    }

    #[test]
    fn keyed_provider_always_has_a_card_regardless_of_detection_or_enable() {
        // MiniMax / a custom endpoint have no binary of their own, so their card
        // always shows — to enter a key or re-enable — even disabled, even with no
        // harness profiles present at all.
        assert!(account_visible(&account("minimax", true), &[]));
        assert!(account_visible(&account("minimax", false), &[]));
        // DeepSeek is now a first-class keyed provider (issue #1127) — its
        // card shows regardless of detection state. Disabled cards stick
        // around so the user can re-enable (see `assemble_meters_keeps_disabled_*`).
        assert!(account_visible(&account("deepseek", true), &[]));
        assert!(account_visible(&account("deepseek", false), &[]));
    }

    #[test]
    fn usage_tracked_recognizes_registered_kinds_and_rejects_generic_provider() {
        assert!(usage_tracked("anthropic"), "representative native meter");
        assert!(usage_tracked("minimax"), "representative keyed meter");
        assert!(!usage_tracked("glm"), "unregistered Generic provider");
    }

    #[test]
    fn poll_ids_require_enabled_visible_and_fetchable() {
        let claude = vec![profile("claude", "anthropic")];
        let accounts = vec![
            account("anthropic", true), // enabled + detected + tracked → in
            account("codex", true),     // tracked but harness undetected → out
            account("minimax", true),   // enabled keyed tracked → in
            account("kimi", true),      // enabled keyed tracked → in (wallet meter)
            // OpenRouter joins the tracked keyed set — opt-in via missing key
            // (defaults to enabled, but `account_visible` lets it through; the
            // fetcher's empty-key path returns `logged_out` until the user adds
            // a key). The `poll_ids` gate here is purely enable+visible+tracked.
            account("openrouter", true),
            // DeepSeek is first-class keyed tracked (issue #1127); the
            // configured-key gate in `assemble_meters` (not `poll_ids`) drops
            // the row until the user adds a key.
            account("deepseek", true),
        ];
        assert_eq!(
            poll_ids(&accounts, &claude),
            vec!["anthropic", "minimax", "kimi", "openrouter", "deepseek"]
        );
    }

    #[test]
    fn poll_ids_excludes_a_disabled_but_visible_tracked_provider() {
        // A disabled MiniMax keeps its card (keyed → visible) but must not be
        // polled — the enable toggle gates the network fetch, not the card.
        assert!(poll_ids(&[account("minimax", false)], &[]).is_empty());
        // A disabled-but-detected Anthropic likewise isn't polled.
        let claude = vec![profile("claude", "anthropic")];
        assert!(poll_ids(&[account("anthropic", false)], &claude).is_empty());
    }

    #[test]
    fn assemble_meters_drops_undetected_natives_and_marks_generic_untracked() {
        let claude = vec![profile("claude", "anthropic")];
        let accounts = vec![
            account("anthropic", true),
            account("codex", true), // undetected harness → excluded entirely (AC1)
            account("minimax", true),
            // A *true* Generic provider (custom id, no first-class fetcher)
            // — included with `usage_tracked = false` (AC4). `glm` stands in
            // for the class: `deepseek` was a Generic before issue #1127
            // promoted it to first-class keyed tracked, so we can't reuse it
            // here without confusing the AC4 contract.
            account("glm", true),
        ];
        let mut usages = HashMap::new();
        usages.insert("anthropic".to_string(), usage("anthropic"));
        usages.insert("minimax".to_string(), usage("minimax"));

        let rows = assemble_meters(&accounts, &claude, &usages, &outcomes_from_usages(&usages), &HashSet::new(), &HashMap::new());
        let ids: Vec<_> = rows.iter().map(|r| r.provider.as_str()).collect();
        assert_eq!(ids, vec!["anthropic", "minimax", "glm"]);

        let anthropic = &rows[0];
        assert!(anthropic.usage_tracked);
        assert!(anthropic.usage.is_some());

        let glm = rows.iter().find(|r| r.provider == "glm").unwrap();
        assert!(!glm.usage_tracked);
        assert!(glm.usage.is_none());
    }

    #[test]
    fn assemble_meters_keeps_a_disabled_detected_native_card_with_no_usage() {
        // Disabling a detected provider hides its meter (not polled) but keeps its
        // card so the user can re-enable it — the row stays, with usage None.
        let claude = vec![profile("claude", "anthropic")];
        let rows = assemble_meters(
            &[account("anthropic", false)],
            &claude,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            &HashMap::new(),
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].provider, "anthropic");
        assert!(rows[0].usage_tracked);
        assert!(rows[0].usage.is_none());
    }

    #[test]
    fn one_credential_proxied_through_many_harnesses_is_one_meter() {
        // Usage follows the credential, not the harness pairing (AC3): the same
        // Anthropic account reachable via two harness profiles still yields a
        // single row — there's one account, so it's counted once.
        let profiles = vec![
            profile("claude", "anthropic"),
            profile("claude-alt", "anthropic"),
        ];
        let accounts = vec![account("anthropic", true)];
        // The no-credential gate now requires a logged-in usage entry for
        // the row to surface; an empty usages map would drop it (matching
        // the user contract for unconfigured providers).
        let mut usages = HashMap::new();
        usages.insert("anthropic".to_string(), usage("anthropic"));
        let rows = assemble_meters(&accounts, &profiles, &usages, &outcomes_from_usages(&usages), &HashSet::new(), &HashMap::new());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].provider, "anthropic");
    }

    // ── No-credential gate (user contract: "I shouldn't see a meter for a
    //    provider I haven't added credentials for") ────────────────────────

    fn logged_out_usage(provider: &str) -> ProviderUsage {
        ProviderUsage {
            provider: provider.to_string(),
            logged_in: false,
            windows: Vec::new(),
            balance: None,
            meters: vec![],
            detail: None,
            error: Some(format!("No API key configured for {provider}")),
        }
    }

    #[test]
    fn assemble_meters_drops_enabled_keyed_provider_without_credentials() {
        // User contract: "If we haven't added credentials for a provider I
        // shouldn't see the usage meter for that provider at all in the
        // usage Probe pane." A fresh user has every built-in enabled but
        // no API keys; MiniMax/OpenRouter must NOT show up as "No API key"
        // cards on the glance surface.
        let mut usages = HashMap::new();
        usages.insert("minimax".to_string(), logged_out_usage("minimax"));
        usages.insert("openrouter".to_string(), logged_out_usage("openrouter"));
        let mut outcomes = HashMap::new();
        outcomes.insert("minimax".to_string(), no_credential_outcome("minimax"));
        outcomes.insert("openrouter".to_string(), no_credential_outcome("openrouter"));
        let rows = assemble_meters(
            &[account("minimax", true), account("openrouter", true)],
            &[],
            &usages,
            &outcomes,
            &HashSet::new(),
            &HashMap::new(),
        );
        assert!(
            rows.is_empty(),
            "keyed providers without credentials must be dropped, got: {rows:?}"
        );
    }

    #[test]
    fn assemble_meters_drops_enabled_native_provider_when_not_logged_in() {
        // Same contract for self-auth native providers: if the credential
        // file is absent (e.g. user hasn't run `claude login` yet) the
        // meter row must be dropped, not rendered as "Not logged in".
        // Detection gating already excludes uninstalled harnesses; this
        // is the second half — exclude detected-but-not-logged-in.
        let claude = vec![profile("claude", "anthropic")];
        let mut usages = HashMap::new();
        usages.insert("anthropic".to_string(), logged_out_usage("anthropic"));
        let mut outcomes = HashMap::new();
        outcomes.insert(
            "anthropic".to_string(),
            no_credential_outcome("anthropic"),
        );
        let rows = assemble_meters(
            &[account("anthropic", true)],
            &claude,
            &usages,
            &outcomes,
            &HashSet::new(),
            &HashMap::new(),
        );
        assert!(
            rows.is_empty(),
            "native providers without credentials must be dropped, got: {rows:?}"
        );
    }

    #[test]
    fn assemble_meters_keeps_keyed_provider_with_rejected_key() {
        // CRITICAL regression guard (code review finding [0]): Kimi and
        // OpenRouter return `logged_in = false` on HTTP 401/403 — i.e.
        // when the stored key has been revoked, expired, or mistyped. A
        // naive `drop_on_logged_in_false` gate would silently hide the row
        // for those users with no in-tab signal to re-enter their key.
        // The configured_keys parameter lets us keep the row when the
        // account HAS a key configured (so <UsagePanel> can render the
        // existing "Invalid API key" copy and the user has a path back
        // to Settings).
        let mut usages = HashMap::new();
        usages.insert("kimi".to_string(), logged_out_usage("kimi"));
        usages.insert("openrouter".to_string(), logged_out_usage("openrouter"));
        // Issue #1745: the kept rows are `Rejected` outcomes (the wire
// envelope is `logged_out`, but the projection of a `Rejected` is
// identical and the test cares about the affordance being visible, not
// about which variant was minted). The gate's keep() predicate keeps
// `Rejected` when a key is configured.
        let mut outcomes = HashMap::new();
        outcomes.insert(
            "kimi".to_string(),
            UsageOutcome::Rejected {
                hint: "Invalid API key".into(),
            },
        );
        outcomes.insert(
            "openrouter".to_string(),
            UsageOutcome::Rejected {
                hint: "Invalid API key".into(),
            },
        );
        let mut configured_keys = HashSet::new();
        configured_keys.insert("kimi".to_string());
        configured_keys.insert("openrouter".to_string());
        let rows = assemble_meters(
            &[account("kimi", true), account("openrouter", true)],
            &[],
            &usages,
            &outcomes,
            &configured_keys,
            &HashMap::new(),
        );
        assert_eq!(
            rows.len(),
            2,
            "keyed providers WITH a configured key must keep their row even when the API rejected the key (got: {rows:?})"
        );
        let ids: Vec<&str> = rows.iter().map(|r| r.provider.as_str()).collect();
        assert!(ids.contains(&"kimi"));
        assert!(ids.contains(&"openrouter"));
        // The logged_out usage is forwarded so the UI can render
        // "Invalid API key" — the user-facing discoverability signal.
        for row in &rows {
            assert!(!row.usage.as_ref().unwrap().logged_in);
            assert!(row.usage.as_ref().unwrap().error.is_some());
        }
    }

    #[test]
    fn assemble_meters_drops_enabled_tracked_provider_missing_from_usages_map() {
        // Regression guard for the `?` operator branch: a polled provider
        // whose entry is absent from `usages` (e.g. a transient fetch
        // failure that didn't even produce a `logged_out` envelope) is
        // dropped silently rather than rendered as "Unable to load usage
        // data". Without this test, a future refactor that swapped `?`
        // back to `usages.get(&a.id).cloned()` would silently reintroduce
        // the stale "Unable to load usage data" fallback path.
        let rows = assemble_meters(
            &[account("minimax", true)],
            &[],
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            &HashMap::new(),
        );
        assert!(
            rows.is_empty(),
            "enabled+tracked with no usages entry must be dropped, got: {rows:?}"
        );
    }

    #[test]
    fn assemble_meters_keeps_disabled_keyed_provider_as_a_card() {
        // Regression guard for the Settings-side "Disabled" card. A user
        // who explicitly disables a keyed provider still needs its card
        // visible so they can re-enable — but the meter row carries
        // `usage = None` so <UsagePanel> renders "Disabled" (not "No API
        // key"). This is the orthogonal axis to the no-credential gate:
        // disabled = "user choice, keep the card", no-credential = "drop
        // entirely".
        let rows = assemble_meters(
            &[account("minimax", false)],
            &[],
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            &HashMap::new(),
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].provider, "minimax");
        assert!(rows[0].usage.is_none(), "disabled card carries no usage");
    }

    #[test]
    fn muse_code_card_is_gated_on_the_muse_harness() {
        let muse = vec![profile("muse", "muse")];
        assert!(account_visible(&account("muse-code", true), &muse));
        assert!(!account_visible(&account("muse-code", true), &[]));
        assert!(usage_tracked("muse-code"));
        assert_eq!(poll_ids(&[account("muse-code", true)], &muse), vec!["muse-code"]);
        assert!(poll_ids(&[account("muse-code", true)], &[]).is_empty());
    }

    #[test]
    fn muse_code_no_credential_outcome_drops_the_row_just_like_grok_and_agy() {
        // Issue #1745 acceptance criterion #3: this is the load-bearing
        // test that pins the bug pre-#1745 (Muse's unconfigured row
        // stayed on the usage surface and rendered red error text) and
        // pins the fix post-#1745 (Muse's no-credential outcome drops
        // the row, identical to Grok and Agy's no-credential case).
        let muse = vec![profile("muse", "muse")];
        let no_credential = UsageOutcome::NoCredential {
            hint: "Muse login missing. Run muse login again.".into(),
        };

        // The projection of `NoCredential` is the same wire envelope that
        // `logged_out()` would have produced — `logged_in: false,
        // error: Some(hint), meters: []`. Pin it here so a future change
        // to the projection cannot silently re-add the `UsageMeter::Unavailable`
        // sentinel (the dead-branch combination pre-#1745).
        let projected = no_credential.clone().into_usage("muse-code");
        assert!(!projected.logged_in);
        assert_eq!(
            projected.error.as_deref(),
            Some("Muse login missing. Run muse login again.")
        );
        assert!(projected.meters.is_empty());

        let mut usages = HashMap::new();
        usages.insert("muse-code".to_string(), projected);
        let mut outcomes = HashMap::new();
        outcomes.insert("muse-code".to_string(), no_credential);
        let rows = assemble_meters(
            &[account("muse-code", true)],
            &muse,
            &usages,
            &outcomes,
            &HashSet::new(),
            &HashMap::new(),
        );
        assert!(
            rows.is_empty(),
            "Muse Code's no-credential case must drop the row (issue #1745), got: {rows:?}"
        );
    }

    /// Issue #1745 acceptance criterion #3: cross-provider equality.
    /// The no-credential wire envelope for muse-code, grok, and agy must
    /// be identical — same field set, same values. Without this, a
    /// future adapter drift re-introduces the cross-provider inconsistency
    /// the seam exists to prevent.
    #[test]
    fn no_credential_envelope_is_identical_across_providers() {
        let muse = UsageOutcome::NoCredential {
            hint: "Muse login missing.".into(),
        }
        .into_usage("muse-code");
        let grok = UsageOutcome::NoCredential {
            hint: "Grok auth missing.".into(),
        }
        .into_usage("grok");
        let agy = UsageOutcome::NoCredential {
            hint: "Antigravity OAuth missing.".into(),
        }
        .into_usage("agy");

        // Compare the structural shape, not the provider-specific copy.
        for (
            (muse_field, muse_value),
            (grok_field, grok_value),
            (agy_field, agy_value),
        ) in [
            (
                ("logged_in", muse.logged_in),
                ("logged_in", grok.logged_in),
                ("logged_in", agy.logged_in),
            ),
            (
                ("windows", muse.windows.is_empty()),
                ("windows", grok.windows.is_empty()),
                ("windows", agy.windows.is_empty()),
            ),
            (
                ("balance", muse.balance.is_none()),
                ("balance", grok.balance.is_none()),
                ("balance", agy.balance.is_none()),
            ),
            (
                ("meters", muse.meters.is_empty()),
                ("meters", grok.meters.is_empty()),
                ("meters", agy.meters.is_empty()),
            ),
            (
                ("detail", muse.detail.is_none()),
                ("detail", grok.detail.is_none()),
                ("detail", agy.detail.is_none()),
            ),
            (
                ("error_is_some", muse.error.is_some()),
                ("error_is_some", grok.error.is_some()),
                ("error_is_some", agy.error.is_some()),
            ),
        ] {
            assert_eq!(
                muse_field, grok_field,
                "field name must match across providers"
            );
            assert_eq!(
                muse_field, agy_field,
                "field name must match across providers"
            );
            assert_eq!(
                muse_value, grok_value,
                "field {muse_field} must match across providers: muse {muse_value:?} vs grok {grok_value:?}"
            );
            assert_eq!(
                muse_value, agy_value,
                "field {muse_field} must match across providers: muse {muse_value:?} vs agy {agy_value:?}"
            );
        }
    }

    // ── Last-known fallback (ADR-0037) ─────────────────────────────────────

    /// A reading the provider reported previously, as the durable store hands it
    /// back: the wire triple plus the instant it was fetched.
    fn remembered(provider: &str, used_percent: f64, cached_at: i64) -> (String, LastKnown) {
        let mut cached = usage(provider);
        cached.windows.push(usage::UsageWindow {
            label: "Weekly".to_string(),
            used_percent: Some(used_percent),
            resets_at: None,
        });
        (
            provider.to_string(),
            LastKnown {
                usage: cached,
                cached_at,
            },
        )
    }

    fn unavailable_usage(provider: &str, reason: &str) -> ProviderUsage {
        ProviderUsage {
            provider: provider.to_string(),
            logged_in: true,
            windows: Vec::new(),
            balance: None,
            meters: vec![],
            detail: None,
            error: Some(reason.to_string()),
        }
    }

    /// The load-bearing case: Grok/Antigravity/Muse are not logged into yet
    /// today, so the fetch returns `NoCredential` and the row would be hidden —
    /// instead it shows the last reading the provider did report, stamped so the
    /// UI can label it as last known.
    #[test]
    fn assemble_meters_serves_the_last_known_reading_when_the_row_would_be_hidden() {
        let grok = vec![profile("grok", "grok")];
        let mut usages = HashMap::new();
        usages.insert("grok".to_string(), logged_out_usage("grok"));
        let mut outcomes = HashMap::new();
        outcomes.insert("grok".to_string(), no_credential_outcome("grok"));
        let previous: HashMap<String, LastKnown> = [remembered("grok", 72.5, 1_700_000_000)]
            .into_iter()
            .collect();

        let rows = assemble_meters(
            &[account("grok", true)],
            &grok,
            &usages,
            &outcomes,
            &HashSet::new(),
            &previous,
        );

        assert_eq!(
            rows.len(),
            1,
            "a remembered reading must keep the row visible, got: {rows:?}"
        );
        let row = &rows[0];
        assert_eq!(
            row.cached_at,
            Some(1_700_000_000),
            "the row must be stamped with when the reading was fetched, not now"
        );
        let shown = row.usage.as_ref().expect("fallback usage");
        assert!(shown.logged_in);
        assert!(shown.error.is_none(), "the failure envelope must not leak through");
        assert_eq!(shown.windows[0].used_percent, Some(72.5));
    }

    #[test]
    fn assemble_meters_drops_the_row_when_nothing_was_remembered() {
        // The documented no-cache behaviour: nothing remembered → hidden, exactly
        // as before the fallback existed.
        let grok = vec![profile("grok", "grok")];
        let mut usages = HashMap::new();
        usages.insert("grok".to_string(), logged_out_usage("grok"));
        let mut outcomes = HashMap::new();
        outcomes.insert("grok".to_string(), no_credential_outcome("grok"));

        let rows = assemble_meters(
            &[account("grok", true)],
            &grok,
            &usages,
            &outcomes,
            &HashSet::new(),
            &HashMap::new(),
        );
        assert!(rows.is_empty(), "nothing remembered means no row: {rows:?}");
    }

    /// A transient failure with a remembered reading serves the cached row:
    /// a slightly outdated meter beats a bare error (the Muse
    /// "did not report subscription usage" case). The row is stamped so the
    /// UI labels it as last known rather than live.
    #[test]
    fn assemble_meters_serves_last_known_for_a_transient_failure_when_remembered() {
        for outcome in [
            UsageOutcome::Unavailable {
                reason: "Muse did not report subscription usage.".into(),
            },
            UsageOutcome::RateLimited {
                reason: "Rate limited.".into(),
            },
        ] {
            let muse = vec![profile("muse", "muse")];
            let mut usages = HashMap::new();
            usages.insert(
                "muse-code".to_string(),
                unavailable_usage("muse-code", "Muse did not report subscription usage."),
            );
            let mut outcomes = HashMap::new();
            outcomes.insert("muse-code".to_string(), outcome);
            let previous: HashMap<String, LastKnown> =
                [remembered("muse-code", 72.5, 1_700_000_000)]
                    .into_iter()
                    .collect();

            let rows = assemble_meters(
                &[account("muse-code", true)],
                &muse,
                &usages,
                &outcomes,
                &HashSet::new(),
                &previous,
            );

            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].cached_at, Some(1_700_000_000));
            let shown = rows[0].usage.as_ref().unwrap();
            assert_eq!(shown.windows[0].used_percent, Some(72.5));
        }
    }

    /// A transient failure with nothing remembered is a live signal the user
    /// can act on; the row keeps showing the error (the with-cache case is
    /// pinned by the test above).
    #[test]
    fn assemble_meters_keeps_the_live_error_for_a_transient_failure() {
        let grok = vec![profile("grok", "grok")];
        let mut usages = HashMap::new();
        usages.insert(
            "grok".to_string(),
            unavailable_usage("grok", "API error 500: upstream down"),
        );
        let mut outcomes = HashMap::new();
        outcomes.insert(
            "grok".to_string(),
            UsageOutcome::Unavailable {
                reason: "API error 500: upstream down".into(),
            },
        );
        let previous: HashMap<String, LastKnown> = HashMap::new();

        let rows = assemble_meters(
            &[account("grok", true)],
            &grok,
            &usages,
            &outcomes,
            &HashSet::new(),
            &previous,
        );

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cached_at, None, "a kept row is not a fallback row");
        let shown = rows[0].usage.as_ref().unwrap();
        assert_eq!(shown.error.as_deref(), Some("API error 500: upstream down"));
        assert!(
            shown.windows.is_empty(),
            "the remembered reading must not replace the live error"
        );
    }

    /// The "Invalid API key" prompt is the user's route back to Settings. A
    /// remembered reading must never stand in for it.
    #[test]
    fn assemble_meters_does_not_cover_a_rejected_key_with_a_stale_reading() {
        let mut usages = HashMap::new();
        let mut rejected = logged_out_usage("kimi");
        rejected.error = Some("Invalid API key".to_string());
        usages.insert("kimi".to_string(), rejected);
        let mut outcomes = HashMap::new();
        outcomes.insert(
            "kimi".to_string(),
            UsageOutcome::Rejected {
                hint: "Invalid API key".into(),
            },
        );
        let configured_keys: HashSet<String> = ["kimi".to_string()].into_iter().collect();
        let previous: HashMap<String, LastKnown> = [remembered("kimi", 12.0, 1_700_000_000)]
            .into_iter()
            .collect();

        let rows = assemble_meters(
            &[account("kimi", true)],
            &[],
            &usages,
            &outcomes,
            &configured_keys,
            &previous,
        );

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cached_at, None, "a rejected key is not a fallback row");
        let shown = rows[0].usage.as_ref().unwrap();
        assert_eq!(shown.error.as_deref(), Some("Invalid API key"));
        assert!(
            shown.windows.is_empty(),
            "the remembered reading must not stand in for the re-entry prompt"
        );
    }

    /// Issue #1073: a cache entry must never gate or discard a successful live
    /// probe.
    #[test]
    fn assemble_meters_never_prefers_a_remembered_reading_over_a_live_one() {
        let grok = vec![profile("grok", "grok")];
        let (_, stale) = remembered("grok", 99.0, 1_700_000_000);
        let mut usages = HashMap::new();
        usages.insert("grok".to_string(), usage("grok"));
        usages.get_mut("grok").unwrap().windows.push(usage::UsageWindow {
            label: "Weekly".to_string(),
            used_percent: Some(5.0),
            resets_at: None,
        });
        let mut outcomes = HashMap::new();
        outcomes.insert("grok".to_string(), reading_outcome("grok"));
        let previous: HashMap<String, LastKnown> = [("grok".to_string(), stale)]
            .into_iter()
            .collect();

        let rows = assemble_meters(
            &[account("grok", true)],
            &grok,
            &usages,
            &outcomes,
            &HashSet::new(),
            &previous,
        );

        assert_eq!(rows[0].cached_at, None, "a live row is never stamped stale");
        assert_eq!(
            rows[0].usage.as_ref().unwrap().windows[0].used_percent,
            Some(5.0),
            "the live reading wins over the remembered one"
        );
    }
}
