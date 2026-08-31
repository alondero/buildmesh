//! Pairing resolution — stored-pairing lookups, attach-form defaults, ordering.

use super::super::compatibility::pairing_can_potentially_match;
use super::super::model::{ApiSurface, ModelTiers, ProxiedProviderOrder, ProviderAccount, ProviderPairing};
use super::super::storage::{load, save};
use super::accounts::provider_accounts;
use super::catalog::{first_class_surfaces, harness_surface, keyed_first_class_template, provider_surfaces};

/// Dedupe an id sequence keeping the first occurrence of each id, preserving
/// order. A malformed caller (or hand-edited prefs) sending `[claude, claude]`
/// would otherwise persist a duplicate that shifts every later harness's index.
pub(super) fn dedupe_keeping_first(ids: impl Iterator<Item = String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    ids.filter(|id| seen.insert(id.clone())).collect()
}

/// The stored **Proxied Provider** pairings in preferences (ADR-0025 / issue
/// #576). A load failure is logged and treated as "no stored pairings".
pub fn provider_pairings() -> Vec<ProviderPairing> {
    match load() {
        Ok(prefs) => prefs.provider_pairings,
        Err(e) => {
            tracing::warn!("preferences::provider_pairings load failed, using none: {}", e);
            Vec::new()
        }
    }
}

/// The full stored per-harness **Proxied Provider** child orders (issue
/// #577). One [`ProxiedProviderOrder`] per harness the user has reordered;
/// a harness without an entry keeps its natural order (the backend's
/// `order_proxied_children` falls through when the per-harness lookup is
/// `None`). A load failure is logged and treated as "no stored order".
pub fn proxied_provider_order() -> Vec<ProxiedProviderOrder> {
    match load() {
        Ok(prefs) => prefs.proxied_provider_order,
        Err(e) => {
            tracing::warn!(
                "preferences::proxied_provider_order load failed, using none: {}",
                e
            );
            Vec::new()
        }
    }
}

/// The stored per-harness child order, if any (issue #577). `None` means
/// "no stored preference" — the backend's `order_proxied_children` falls
/// through to natural insertion order. The pair-id comparator
/// `(harness_id, provider_id)` is identical to the pairing key the rest of
/// the codebase uses, so this lookup is a one-liner for the spawn menu and
/// the harness-config page alike.
///
/// `pub` for the single-harness lookup symmetry with the full-list
/// [`proxied_provider_order`] getter and the [`set_proxied_provider_order`]
/// setter — the spawn-menu read path uses [`proxied_provider_order`] (a
/// single `HashMap` lookup) for performance, but a UI surface that wants
/// "what's the order under harness X?" can reach for this without parsing
/// the full vector.
#[allow(dead_code)]
pub fn proxied_order_for(harness_id: &str) -> Option<Vec<String>> {
    proxied_provider_order()
        .into_iter()
        .find(|o| o.harness_id == harness_id)
        .map(|o| o.provider_ids)
}

/// Persist the **Proxied Provider** child order for a single harness (issue
/// #577). Upserts by `harness_id`: a re-set replaces the prior entry in
/// place; a new harness appends. An empty `provider_ids` is normalised to
/// "drop the entry" — the natural order is then re-derived on read, so a
/// detach-reattach sequence can't be confused by a stale empty slot.
///
/// Defensive filters on the incoming list (mirrors `set_harness_order`'s
/// dedupe/keep-first pattern):
///   * **Dedup** — first occurrence wins; later duplicates are dropped
///     (a malformed UI send with `[a, b, a]` doesn't shift every later
///     provider's index on persist).
///   * **Drop unknown account ids** — any id that isn't a registered
///     [`ProviderAccount`] (built-in or custom) is silently dropped. The
///     ordering seam would never render it, and persisting it would let a
///     stale UI send pollute the stored preferences. Note we validate
///     against `provider_accounts()` rather than `provider_pairings()`:
///     the order is meaningful for any account the user could attach under
///     this harness, even before the pairing exists (a user can rearrange
///     after a future attach and the slot is reserved).
pub fn set_proxied_provider_order(harness_id: String, provider_ids: Vec<String>) -> Result<(), String> {
    let mut prefs = load()?;
    // Known = effective accounts ∪ keyed first-class catalog (ADR-0025: keyed
    // rows may not be materialised yet but are still attachable / orderable).
    let mut known_ids: std::collections::HashSet<String> = provider_accounts()
        .into_iter()
        .map(|a| a.id)
        .collect();
    for a in super::catalog::keyed_first_class_catalog() {
        known_ids.insert(a.id);
    }
    let incoming: Vec<String> = dedupe_keeping_first(provider_ids.into_iter())
        .into_iter()
        .filter(|id| known_ids.contains(id))
        .collect();
    let existing = prefs
        .proxied_provider_order
        .iter_mut()
        .find(|o| o.harness_id == harness_id);
    match existing {
        Some(entry) if incoming.is_empty() => {
            // Empty list = the user cleared their order preference. Drop
            // the entry entirely so the next read returns `None` and the
            // backend falls through to natural order — a stored empty
            // would silently no-op the ordering seam.
            let _ = entry; // `entry` is borrowed only for the empty-list check
            prefs.proxied_provider_order.retain(|o| o.harness_id != harness_id);
        }
        Some(entry) => entry.provider_ids = incoming,
        None if !incoming.is_empty() => prefs.proxied_provider_order.push(ProxiedProviderOrder {
            harness_id,
            provider_ids: incoming,
        }),
        None => { /* nothing to persist */ }
    }
    save(prefs)
}

/// Resolve the **stored** pairing for spawn / env (ADR-0025). Spawn is
/// stored-only — a composite spawn id without a stored attach yields `None`
/// (empty env), so a keyless account never auto-spawns.
pub(crate) fn resolve_pairing(
    harness_id: &str,
    account: &ProviderAccount,
    stored: &[ProviderPairing],
) -> Option<ProviderPairing> {
    stored
        .iter()
        .find(|p| p.harness_id == harness_id && p.provider_id == account.id)
        .cloned()
}

pub fn resolve_stored_pairing_and_account(
    spawn_option_id: &str,
) -> Result<Option<(ProviderPairing, ProviderAccount)>, String> {
    let (harness_id, provider_id) =
        crate::agent::provider::parse_spawn_option_id(spawn_option_id);
    let Some(provider_id) = provider_id else {
        return Ok(None);
    };
    let accounts = provider_accounts();
    let account = accounts
        .into_iter()
        .find(|account| account.id == provider_id)
        .ok_or_else(|| format!("provider account '{provider_id}' is missing"))?;
    let pairing = resolve_pairing(harness_id, &account, &provider_pairings())
        .ok_or_else(|| format!("pairing '{harness_id}:{provider_id}' is missing"))?;
    Ok(Some((pairing, account)))
}

/// Attach-form defaults for `(harness, provider)`: stored pairing wins, else
/// first-class published endpoint for the harness surface. Generics without a
/// stored pairing return a surface-only shell (`base_url = None`) so the UI
/// can still show the form; the attach command requires a non-empty URL.
pub(super) fn attach_pairing_defaults(
    harness_id: &str,
    account: &ProviderAccount,
    stored: &[ProviderPairing],
    surface_of: impl Fn(&str) -> Option<ApiSurface>,
) -> Option<ProviderPairing> {
    if let Some(p) = resolve_pairing(harness_id, account, stored) {
        return Some(p);
    }
    let surface = surface_of(harness_id)?;
    if let Some(ep) = first_class_surfaces(&account.id)
        .into_iter()
        .find(|e| e.surface == surface)
    {
        return Some(ProviderPairing {
            harness_id: harness_id.to_string(),
            provider_id: account.id.clone(),
            surface,
            base_url: Some(ep.base_url),
            model_tiers: ep.model_tiers,
        });
    }
    // Generic: surface from harness, empty endpoint for the user to fill.
    if account.claude_compatible {
        return Some(ProviderPairing {
            harness_id: harness_id.to_string(),
            provider_id: account.id.clone(),
            surface,
            base_url: None,
            model_tiers: ModelTiers::default(),
        });
    }
    None
}

/// The full set of **Proxied Provider** pairings to render in the Spawn Menu
/// (ADR-0025 / issue #576): **stored pairings only**, filtered to proxiable
/// accounts. No derived default Anthropic pairing on key alone. Pure (no
/// disk/globals) — the unit-test seam for the menu derivation.
///
/// "Proxiable" = enabled, Claude-compatible, and keyed (non-empty API key).
pub(crate) fn effective_pairings(
    accounts: &[ProviderAccount],
    stored: &[ProviderPairing],
) -> Vec<ProviderPairing> {
    let keyed = |a: &ProviderAccount| a.api_key.as_deref().is_some_and(|k| !k.is_empty());
    let is_proxiable = |a: &ProviderAccount| a.enabled && a.claude_compatible && keyed(a);
    let proxiable_ids: std::collections::HashSet<&str> = accounts
        .iter()
        .filter(|a| is_proxiable(a))
        .map(|a| a.id.as_str())
        .collect();
    stored
        .iter()
        .filter(|p| proxiable_ids.contains(p.provider_id.as_str()))
        .cloned()
        .collect()
}

/// The full effective pairing set (stored only, ADR-0025) read from disk —
/// the harness-config page's "what's attached to each harness" source.
pub fn effective_provider_pairings() -> Vec<ProviderPairing> {
    effective_pairings(&provider_accounts(), &provider_pairings())
}

/// The **Model Providers** that can be attached to `harness_id` — those whose
/// published/declared surfaces include the surface the harness speaks (issue
/// #576). Drives the surface-matched "Add proxied provider" picker so only
/// compatible providers are offered. Empty for a native-only harness (Terminal,
/// Antigravity, OpenCode) that speaks no proxy surface.
pub fn compatible_providers_for_harness(harness_id: &str) -> Vec<ProviderAccount> {
    let Some(surface) = harness_surface(harness_id) else {
        return Vec::new();
    };
    let stored = provider_pairings();
    provider_accounts()
        .into_iter()
        .filter(|account| {
            if !provider_surfaces(account).contains(&surface) {
                return false;
            }
            attach_pairing_defaults(harness_id, account, &stored, |_| Some(surface))
                .is_some_and(|pairing| pairing_can_potentially_match(&pairing))
        })
        .collect()
}

/// The pairing the attach flow should store when proxying `provider_id` through
/// `harness_id` (issue #576 / ADR-0025): a stored pairing wins (idempotent
/// re-attach), else the published first-class default for the harness's surface.
/// Looks up effective accounts first, then the keyed first-class catalog so an
/// as-yet-unmaterialised MiniMax/Kimi/OpenRouter still prefills. `None` when
/// the provider is incompatible with the harness's surface.
pub fn pairing_for(harness_id: &str, provider_id: &str) -> Option<ProviderPairing> {
    let accounts = provider_accounts();
    let account = accounts
        .iter()
        .find(|a| a.id == provider_id)
        .cloned()
        .or_else(|| keyed_first_class_template(provider_id))?;
    attach_pairing_defaults(harness_id, &account, &provider_pairings(), harness_surface)
}