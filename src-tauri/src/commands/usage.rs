//! Tauri commands for provider usage fetching.

use crate::preferences::{self, HarnessProfile, ProviderAccount};
use crate::services::usage::{self, ProviderMeters, ProviderUsage};
use std::collections::{HashMap, HashSet};
use tauri::command;

/// Provider ids Buildmesh can actually fetch usage for. A visible account whose
/// id isn't here — a **Generic Model Provider** (custom endpoint) — is
/// configurable but has no usage endpoint, so it renders an explicit "usage not
/// tracked" state instead.
///
/// `openai` joins the keyed tracked set per issue #1109 / ADR-0026: the
/// Organization Costs API is admin-scoped, so a `sk-proj-…` key degrades
/// gracefully (logged-in, detail explains) rather than failing the row.
/// `deepseek` joins in issue #1127 — its `/user/balance` endpoint is keyed
/// (user-supplied DeepSeek API key), PayAsYouGo, and returns a
/// `BillingBalance` (no usage windows). The no-credential gate in
/// [`assemble_meters`] drops the row until the user stores a key, matching
/// the user contract for keyed providers.
const FETCHABLE: [&str; 12] = [
    "anthropic", "codex", "cursor", "minimax", "agy", "kimi", "openrouter", "grok", "opencode", "commandcode", "openai", "deepseek",
];

/// Map a self-authenticating **native** provider account to the harness whose
/// *installation* gates its subscription meter: `anthropic`↔Claude Code (harness
/// id `anthropic`), `codex`↔Codex, `cursor`↔Cursor, `agy`↔Antigravity, `opencode`↔OpenCode. A keyed provider
/// (MiniMax/Kimi/OpenRouter/custom Claude-compatible) returns `None` — it's
/// gated on the user enabling it, not on a detected binary. The Kimi Code CLI
/// Agent Harness itself is registered separately in `HarnessProfile` /
/// `Provider::Kimi` / `KIMI` adapter; the `kimi` row here is the
/// First-class Model Provider for the Moonshot Kimi LLM endpoint.
fn native_harness_for(account_id: &str) -> Option<&'static str> {
    match account_id {
        "anthropic" => Some("anthropic"),
        "codex" => Some("codex"),
        "cursor" => Some("cursor"),
        "agy" => Some("agy"),
        "grok" => Some("grok"),
        "opencode" => Some("opencode"),
        "commandcode" => Some("commandcode"),
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
    match native_harness_for(&account.id) {
        Some(harness) => harness_detected(harness, profiles),
        None => true,
    }
}

/// Whether Buildmesh ships a usage fetcher for this provider id. `false` for a
/// **Generic Model Provider** — surfaces as "usage not tracked".
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

/// The keyed provider ids that have a non-empty resolved API key configured
/// (account-level, with the legacy `prefs.minimax_api_key` fallback for
/// MiniMax). Read once per `get_provider_meters` call and threaded into
/// [`assemble_meters`] so the gate can distinguish "credential configured"
/// (keep the row, even if the API rejected the key) from "no credential at
/// all" (drop the row). Reading the legacy fields and `*_api_key_resolved()`
/// helpers here keeps `assemble_meters` pure for unit tests — callers pass
/// the resolved set in.
fn configured_keyed_providers(accounts: &[ProviderAccount]) -> HashSet<String> {
    let mut configured = HashSet::new();
    let has_key = |id: &str| -> bool {
        match id {
            "minimax" => preferences::minimax_api_key_resolved().is_some_and(|k| !k.is_empty()),
            "kimi" => preferences::kimi_api_key_resolved().is_some_and(|k| !k.is_empty()),
            "openrouter" => preferences::openrouter_api_key_resolved().is_some_and(|k| !k.is_empty()),
            "openai" => preferences::openai_api_key_resolved().is_some_and(|k| !k.is_empty()),
            // DeepSeek joins the keyed tracked set in issue #1127 — its
            // `/user/balance` fetcher is keyed (no legacy flat field).
            "deepseek" => preferences::deepseek_api_key_resolved().is_some_and(|k| !k.is_empty()),
            // Custom (Generic) Claude-compatible providers store the key on
            // the account itself — no legacy flat field.
            other => accounts
                .iter()
                .find(|a| a.id == other)
                .and_then(|a| a.api_key.as_deref())
                .is_some_and(|k| !k.is_empty()),
        }
    };
    for id in ["minimax", "kimi", "openrouter", "openai", "deepseek"] {
        if has_key(id) {
            configured.insert(id.to_string());
        }
    }
    // Custom Generic providers keyed on the api_key field.
    for account in accounts {
        if !FETCHABLE.contains(&account.id.as_str())
            && account.api_key.as_deref().is_some_and(|k| !k.is_empty())
        {
            configured.insert(account.id.clone());
        }
    }
    configured
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
/// `usage.logged_in`? Because Kimi (`services::usage.rs:564-566`) and
/// OpenRouter (`services::usage.rs:662-664`) both return
/// `logged_in = false` on HTTP 401/403 — i.e. when the user's stored key
/// has been revoked, expired, or mistyped. Conflating "no credential" with
/// "credential is bad" would silently drop the row for those users with
/// no in-tab signal to re-enter their key. By gating on the *account-level*
/// key presence (the `configured_keys` parameter), the row stays visible
/// when the key exists but the API rejected it — `<UsagePanel>` renders
/// the existing "Invalid API key" copy (UsageRender.tsx:115) and the user
/// has a path back to Settings.
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
fn assemble_meters(
    accounts: &[ProviderAccount],
    profiles: &[HarnessProfile],
    usages: &HashMap<String, ProviderUsage>,
    configured_keys: &HashSet<String>,
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
                });
            }
            // Generic untracked: keep the "Usage not tracked" card.
            if !tracked {
                return Some(ProviderMeters {
                    provider: a.id.clone(),
                    usage_tracked: false,
                    usage: None,
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
            let usage = usages.get(&a.id)?;
            if !usage.logged_in && !configured_keys.contains(&a.id) {
                return None;
            }
            Some(ProviderMeters {
                provider: a.id.clone(),
                usage_tracked: tracked,
                usage: Some(usage.clone()),
            })
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
        "cursor" => usage::cursor_usage(),
        // `minimax_api_key_resolved` already reads the account key then the legacy
        // flat field, so it's the single source of truth here.
        "minimax" => usage::minimax_usage(
            preferences::minimax_api_key_resolved().as_deref().unwrap_or(""),
        ),
        "agy" => usage::agy_usage(),
        // Kimi has no legacy flat field today, but resolving through
        // `kimi_api_key_resolved()` mirrors `minimax_api_key_resolved()` so a
        // future legacy fallback lands in preferences.rs (single seam) rather
        // than this dispatch file. Empty string lets `kimi_usage` surface its
        // own "No API key configured" message.
        "kimi" => usage::kimi_usage(
            preferences::kimi_api_key_resolved()
                .as_deref()
                .unwrap_or(""),
        ),
        // OpenRouter — identical seam pattern: account key only, no legacy flat
        // field. Empty string lets `openrouter_usage` surface its own "No API
        // key configured" message and renders as `usage_tracked` with a logged-
        // out card until the user adds a key.
        "openrouter" => usage::openrouter_usage(
            preferences::openrouter_api_key_resolved()
                .as_deref()
                .unwrap_or(""),
        ),
        "grok" => usage::grok_usage(),
        "opencode" => usage::opencode_usage(),
        "commandcode" => usage::commandcode_usage(),
        // OpenAI — keyed, no legacy flat field. Empty string lets
        // `openai_usage` surface its own "No API key configured" message
        // (mirrors Kimi/OpenRouter). The configured-key gate in
        // `assemble_meters` then drops the row so a fresh user doesn't see a
        // "Not logged in" card before they've added a key.
        "openai" => usage::openai_usage(
            preferences::openai_api_key_resolved()
                .as_deref()
                .unwrap_or(""),
        ),
        // DeepSeek — keyed, no legacy flat field. Mirrors Kimi/OpenRouter:
        // empty string lets `deepseek_usage` surface its own "No API key
        // configured" message and renders as `usage_tracked` with a
        // logged-out card until the user adds a key. The balance endpoint
        // is queried through `usage::deepseek_usage` (issue #1127).
        "deepseek" => usage::deepseek_usage(
            preferences::deepseek_api_key_resolved()
                .as_deref()
                .unwrap_or(""),
        ),
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
    let (profiles, accounts, ids, configured_keys) =
        crate::commands::run_blocking("get_provider_meters_ids", || {
            let profiles = preferences::harness_profiles();
            let accounts = preferences::provider_accounts();
            let ids = poll_ids(&accounts, &profiles);
            // Resolve once which keyed providers have a credential configured —
            // threaded into the gate so a 401/403 from Kimi/OpenRouter doesn't
            // silently hide the row (see `assemble_meters` docstring).
            let configured_keys = configured_keyed_providers(&accounts);
            Ok((profiles, accounts, ids, configured_keys))
        })
        .await?;

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

    Ok(assemble_meters(&accounts, &profiles, &usages, &configured_keys))
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
        // DeepSeek is now a first-class keyed provider (issue #1127) — its
        // card shows regardless of detection state. Disabled cards stick
        // around so the user can re-enable (see `assemble_meters_keeps_disabled_*`).
        assert!(account_visible(&account("deepseek", true), &[]));
        assert!(account_visible(&account("deepseek", false), &[]));
    }

    #[test]
    fn usage_tracked_only_for_providers_with_a_fetcher() {
        for id in ["anthropic", "codex", "cursor", "minimax", "agy", "kimi", "openrouter", "grok", "opencode", "commandcode", "openai", "deepseek"] {
            assert!(usage_tracked(id), "{id} should be tracked");
        }
        // Any Generic provider is untracked.
        let id = "glm";
        assert!(!usage_tracked(id), "{id} should not be tracked");
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
            account("codex", true),    // undetected harness → excluded entirely (AC1)
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

        let rows = assemble_meters(&accounts, &claude, &usages, &HashSet::new());
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
            &HashSet::new(),
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
        let profiles = vec![profile("claude", "anthropic"), profile("claude-alt", "anthropic")];
        let accounts = vec![account("anthropic", true)];
        // The no-credential gate now requires a logged-in usage entry for
        // the row to surface; an empty usages map would drop it (matching
        // the user contract for unconfigured providers).
        let mut usages = HashMap::new();
        usages.insert("anthropic".to_string(), usage("anthropic"));
        let rows = assemble_meters(&accounts, &profiles, &usages, &HashSet::new());
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
        let rows = assemble_meters(
            &[account("minimax", true), account("openrouter", true)],
            &[],
            &usages,
            &HashSet::new(),
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
        let rows = assemble_meters(
            &[account("anthropic", true)],
            &claude,
            &usages,
            &HashSet::new(),
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
        let mut configured_keys = HashSet::new();
        configured_keys.insert("kimi".to_string());
        configured_keys.insert("openrouter".to_string());
        let rows = assemble_meters(
            &[account("kimi", true), account("openrouter", true)],
            &[],
            &usages,
            &configured_keys,
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
            &HashSet::new(),
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
            &HashSet::new(),
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].provider, "minimax");
        assert!(rows[0].usage.is_none(), "disabled card carries no usage");
    }
}
