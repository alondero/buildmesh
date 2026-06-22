//! Tauri commands for provider usage fetching.

use crate::preferences::{self, HarnessProfile, ProviderAccount};
use crate::services::usage::{self, ProviderMeters, ProviderUsage};
use std::collections::HashMap;
use tauri::command;

/// Provider ids Buildmesh can actually fetch usage for. A visible account whose
/// id isn't here — a **Generic Model Provider** (custom endpoint) or a
/// first-class provider without a fetcher yet (Kimi) — is configurable but has no
/// usage endpoint, so it renders an explicit "usage not tracked" state instead.
const FETCHABLE: [&str; 4] = ["anthropic", "codex", "minimax", "agy"];

/// Map a self-authenticating **native** provider account to the harness whose
/// *installation* gates its subscription meter: `anthropic`↔Claude Code (harness
/// id `anthropic`), `codex`↔Codex, `agy`↔Antigravity. A keyed provider
/// (MiniMax/Kimi/custom) returns `None` — it's gated on the user enabling it, not
/// on a detected binary.
fn native_harness_for(account_id: &str) -> Option<&'static str> {
    match account_id {
        "anthropic" => Some("anthropic"),
        "codex" => Some("codex"),
        "agy" => Some("agy"),
        _ => None,
    }
}

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
/// provider (first-class MiniMax/Kimi or a Generic custom endpoint) always has a
/// card so its credential editor and enable toggle stay reachable.
///
/// This is *card* visibility, deliberately independent of `enabled`: a user who
/// disables a provider must keep its card (showing "Disabled") so they can turn
/// it back on — `enabled` gates polling (see [`poll_ids`]), not the card. Pure
/// (no disk/network) so it's the unit-test seam.
fn account_visible(account: &ProviderAccount, profiles: &[HarnessProfile]) -> bool {
    match native_harness_for(&account.id) {
        Some(harness) => harness_detected(harness, profiles),
        None => true,
    }
}

/// Whether Buildmesh ships a usage fetcher for this provider id. `false` for a
/// **Generic Model Provider** and for a first-class provider without a fetcher
/// (Kimi) — both surface as "usage not tracked".
fn usage_tracked(account_id: &str) -> bool {
    FETCHABLE.contains(&account_id)
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

/// Build the Providers-page rows from the gated account set and a map of
/// already-fetched usages (keyed by provider id). Pure so the detection-gating +
/// usage-tracked derivation is unit-tested without touching the network: only
/// visible accounts appear (an uninstalled native harness is dropped entirely),
/// `usage` is populated for tracked providers, and a Generic provider gets
/// `usage_tracked = false` with no usage.
fn assemble_meters(
    accounts: &[ProviderAccount],
    profiles: &[HarnessProfile],
    usages: &HashMap<String, ProviderUsage>,
) -> Vec<ProviderMeters> {
    accounts
        .iter()
        .filter(|a| account_visible(a, profiles))
        .map(|a| {
            let tracked = usage_tracked(&a.id);
            ProviderMeters {
                provider: a.id.clone(),
                usage_tracked: tracked,
                usage: if tracked { usages.get(&a.id).cloned() } else { None },
            }
        })
        .collect()
}

/// Fetches a single provider's usage, serving a fresh cache entry unless
/// `force_refresh` is set, and caching whatever it fetches.
fn cached_or_fetch(provider: &str, force_refresh: bool) -> ProviderUsage {
    if !force_refresh {
        if let Some(cached) = usage::get_cached_usage(provider) {
            return cached;
        }
    }

    let result = match provider {
        "anthropic" => usage::anthropic_usage(),
        "codex" => usage::codex_usage(),
        // `minimax_api_key_resolved` already reads the account key then the legacy
        // flat field, so it's the single source of truth here.
        "minimax" => usage::minimax_usage(
            preferences::minimax_api_key_resolved().as_deref().unwrap_or(""),
        ),
        "agy" => usage::agy_usage(),
        other => unreachable!("cached_or_fetch called with unknown provider: {other}"),
    };

    usage::set_cached_usage(provider, result.clone());
    result
}

/// Returns the detection-gated Providers-page rows: one [`ProviderMeters`] per
/// provider relevant to this host (issue #574). Native subscription meters appear
/// only for installed harnesses; keyed providers only when enabled; Generic
/// providers carry `usage_tracked = false`. Reuses the `ProviderUsage` wire shape.
#[command]
pub async fn get_provider_meters(force_refresh: bool) -> Result<Vec<ProviderMeters>, String> {
    let profiles = preferences::harness_profiles();
    let accounts = preferences::provider_accounts();
    let ids = poll_ids(&accounts, &profiles);

    // Each fetch is a blocking HTTP round-trip to a different vendor; running
    // them serially made the panel wait for the sum of all of them. Fan out on
    // blocking threads and collect into a map keyed by provider id.
    let handles: Vec<_> = ids
        .into_iter()
        .map(|id| {
            tauri::async_runtime::spawn_blocking(move || {
                let usage = cached_or_fetch(&id, force_refresh);
                (id, usage)
            })
        })
        .collect();

    let mut usages: HashMap<String, ProviderUsage> = HashMap::new();
    for handle in handles {
        let (id, usage) = handle
            .await
            .map_err(|e| format!("usage fetch task failed: {}", e))?;
        usages.insert(id, usage);
    }

    Ok(assemble_meters(&accounts, &profiles, &usages))
}

#[command]
pub async fn set_minimax_api_key(key: Option<String>) -> Result<(), String> {
    let mut prefs = preferences::load()?;
    prefs.minimax_api_key = key;
    preferences::save(prefs)?;
    usage::invalidate_cache();
    Ok(())
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
            base_url: None,
            model_tiers: crate::preferences::ModelTiers::default(),
            models: Vec::new(),
        }
    }

    fn profile(id: &str, harness: &str) -> HarnessProfile {
        HarnessProfile {
            id: id.to_string(),
            name: id.to_string(),
            harness: harness.to_string(),
        }
    }

    fn usage(provider: &str) -> ProviderUsage {
        ProviderUsage {
            provider: provider.to_string(),
            logged_in: true,
            windows: Vec::new(),
            balance: None,
            detail: None,
            error: None,
        }
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
        assert!(account_visible(&account("deepseek", false), &[]));
    }

    #[test]
    fn usage_tracked_only_for_providers_with_a_fetcher() {
        for id in ["anthropic", "codex", "minimax", "agy"] {
            assert!(usage_tracked(id), "{id} should be tracked");
        }
        // Kimi (first-class, no fetcher yet) and any Generic provider are untracked.
        for id in ["kimi", "deepseek", "glm"] {
            assert!(!usage_tracked(id), "{id} should not be tracked");
        }
    }

    #[test]
    fn poll_ids_require_enabled_visible_and_fetchable() {
        let claude = vec![profile("claude", "anthropic")];
        let accounts = vec![
            account("anthropic", true), // enabled + detected + tracked → in
            account("codex", true),     // tracked but harness undetected → out
            account("minimax", true),   // enabled keyed tracked → in
            account("kimi", true),      // keyed but no fetcher → out
            account("deepseek", true),  // Generic, no fetcher → out
        ];
        assert_eq!(poll_ids(&accounts, &claude), vec!["anthropic", "minimax"]);
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
            account("codex", true),    // undetected harness → excluded entirely (AC1)
            account("minimax", true),
            account("deepseek", true), // Generic → included, usage_tracked = false (AC4)
        ];
        let mut usages = HashMap::new();
        usages.insert("anthropic".to_string(), usage("anthropic"));
        usages.insert("minimax".to_string(), usage("minimax"));

        let rows = assemble_meters(&accounts, &claude, &usages);
        let ids: Vec<_> = rows.iter().map(|r| r.provider.as_str()).collect();
        assert_eq!(ids, vec!["anthropic", "minimax", "deepseek"]);

        let anthropic = &rows[0];
        assert!(anthropic.usage_tracked);
        assert!(anthropic.usage.is_some());

        let deepseek = rows.iter().find(|r| r.provider == "deepseek").unwrap();
        assert!(!deepseek.usage_tracked);
        assert!(deepseek.usage.is_none());
    }

    #[test]
    fn assemble_meters_keeps_a_disabled_detected_native_card_with_no_usage() {
        // Disabling a detected provider hides its meter (not polled) but keeps its
        // card so the user can re-enable it — the row stays, with usage None.
        let claude = vec![profile("claude", "anthropic")];
        let rows = assemble_meters(&[account("anthropic", false)], &claude, &HashMap::new());
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
        let profiles = vec![profile("claude", "anthropic"), profile("claude-alt", "anthropic")];
        let accounts = vec![account("anthropic", true)];
        let rows = assemble_meters(&accounts, &profiles, &HashMap::new());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].provider, "anthropic");
    }
}
