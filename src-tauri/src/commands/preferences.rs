//! Tauri commands for buildmesh-wide preferences.
//!
//! See `crate::preferences` for the persistence layer.

use crate::preferences::{self, AppPreferences, ModelTiers, ProviderAccount, ProviderPairing};
use tauri::{command, AppHandle, Emitter};

/// Read the persisted buildmesh-wide preferences. Always returns a value —
/// a missing or malformed file yields `AppPreferences::default()`.
#[command]
pub async fn get_app_preferences() -> Result<AppPreferences, String> {
    preferences::load()
}

/// Set the buildmesh-wide default provider. Pass `None` (or an empty string,
/// which is normalised away) to clear the override and restore the hardcoded
/// `claude` fallback (post-#538 unified harness id).
#[command]
pub async fn set_app_default_provider(provider: Option<String>) -> Result<(), String> {
    let mut prefs = preferences::load()?;
    prefs.default_provider = provider.filter(|s| !s.is_empty());
    preferences::save(prefs)
}

/// Set the backend that summaries node PTY output into a slug (issue #824).
/// Distinct from [`set_app_default_provider`]: auto-naming runs on every
/// rename trigger (often), at low content complexity, so it shouldn't
/// inherit an expensive tier the spawned node happens to be on. `None` (or
/// an empty string) **disables auto-naming entirely** — nodes keep their
/// random `adjective-adjective-noun` slugs until the user picks a value.
#[command]
pub async fn set_app_naming_provider(provider: Option<String>) -> Result<(), String> {
    let mut prefs = preferences::load()?;
    prefs.naming_provider = provider.filter(|s| !s.is_empty());
    preferences::save(prefs)
}

/// Set the app-wide autopilot pool size — the cap on concurrently active
/// autopilot nodes across **all** meshes. `None` clears the cap (per-mesh
/// limits alone apply); `Some(0)` pauses all new autopilot spawns. Takes
/// effect on the poller's next pass — running nodes are never killed.
#[command]
pub async fn set_app_autopilot_pool_size(size: Option<u32>) -> Result<(), String> {
    let mut prefs = preferences::load()?;
    prefs.autopilot_pool_size = size;
    preferences::save(prefs)
}

/// Persist the user's spawn-menu harness order (issue #573). `order` is the list
/// of harness-row ids in the desired top-to-bottom order; `Terminal` is filtered
/// out backend-side (it's always forced last). Emits `provider-list-changed` so
/// every spawn surface (sidebar menu, Probe tabs) drops its cached provider list
/// and re-reads the reordered menu — the same cross-component invalidation used
/// by the account commands.
#[command]
pub async fn set_harness_order(app: AppHandle, order: Vec<String>) -> Result<(), String> {
    preferences::set_harness_order(order)?;
    let _ = app.emit("provider-list-changed", ());
    Ok(())
}

/// Persist the **Proxied Provider** child order under one harness (issue
/// #577). `provider_ids` is the top-to-bottom list of `provider_id`s as the
/// user arranged them in the drag list on the harness-config page; only
/// registered [`crate::preferences::ProviderAccount`] ids are persisted
/// (unknown ids are silently dropped — the order seam would never render
/// them anyway, and a stale UI send can't pollute the preferences file).
/// Cross-harness drag is disallowed at the UI layer (each `HarnessCard`
/// is its own `DndContext`), so the harness_id + provider_ids pair is the
/// entire scope the command accepts.
///
/// Emits `provider-list-changed` so every spawn surface (sidebar, Probe
/// tabs, archived-resume, mobile) drops its cached provider list and
/// re-reads the reordered menu — the same invalidation [`set_harness_order`]
/// fires for the harness-level reorder.
#[command]
pub async fn set_proxied_provider_order(
    app: AppHandle,
    harness_id: String,
    provider_ids: Vec<String>,
) -> Result<(), String> {
    preferences::set_proxied_provider_order(harness_id, provider_ids)?;
    let _ = app.emit("provider-list-changed", ());
    Ok(())
}

/// The effective model-provider account list — self-auth built-ins plus any
/// keyed first-class / generic accounts the user has added (ADR-0025).
#[command]
pub async fn get_provider_accounts() -> Result<Vec<ProviderAccount>, String> {
    Ok(preferences::provider_accounts())
}

/// Keyed first-class catalog templates (MiniMax, Kimi, OpenRouter) for the
/// Providers-page "Add provider" picker (ADR-0025). The UI filters out ids
/// already present in [`get_provider_accounts`].
#[command]
pub async fn get_keyed_first_class_catalog() -> Result<Vec<ProviderAccount>, String> {
    Ok(preferences::keyed_first_class_catalog())
}

/// Attach-form defaults for `(harness_id, provider_id)` — first-class published
/// endpoint + tiers when available; `None` when the pair is incompatible
/// (ADR-0025). Does not require a stored pairing.
#[command]
pub async fn get_pairing_defaults(
    harness_id: String,
    provider_id: String,
) -> Result<Option<ProviderPairing>, String> {
    Ok(preferences::pairing_for(&harness_id, &provider_id))
}

/// Create or update a model-provider account (issue #537 / ADR-0025). For a
/// custom (non-built-in) account the row is added; spawn-menu visibility
/// requires an explicit attach under the Harnesses page. Invalidates the
/// usage cache so a changed key/enabled-state is reflected on the next panel
/// refresh.
///
/// Emits `provider-list-changed` so frontend consumers (Sidebar spawn menu,
/// Probe tabs that list provider options) drop their locally-cached provider
/// list and re-read. The tauri.ts `listProviders` cache is also busted in the
/// JS wrapper, but that only helps callers within the same component — other
/// components with their own `providerData` state need an explicit signal.
#[command]
pub async fn upsert_provider_account(app: AppHandle, account: ProviderAccount) -> Result<(), String> {
    let mut prefs = preferences::load()?;
    preferences::upsert_provider_account(&mut prefs, account);
    preferences::save(prefs)?;
    crate::services::usage::invalidate_cache();
    let _ = app.emit("provider-list-changed", ());
    Ok(())
}

/// Remove a stored provider account. Removing a self-auth built-in just
/// reverts it to the code-defined default; keyed-first-class and generic
/// rows are deleted outright (re-adding them starts from the catalog /
/// blank). Stored pairings for the removed id are filtered out at spawn
/// time — detach separately if the goal is hiding the row from the spawn
/// menu only. Emits `provider-list-changed` for cross-component
/// invalidation (same reason as [`upsert_provider_account`]).
#[command]
pub async fn remove_provider_account(app: AppHandle, id: String) -> Result<(), String> {
    let mut prefs = preferences::load()?;
    preferences::remove_provider_account(&mut prefs, &id);
    preferences::save(prefs)?;
    crate::services::usage::invalidate_cache();
    let _ = app.emit("provider-list-changed", ());
    Ok(())
}

// ---------------------------------------------------------------------------
// Proxied Provider pairings (ADR-0016 §4, issue #576)
// ---------------------------------------------------------------------------

/// The full effective set of **Proxied Provider** pairings — stored pairings
/// for proxiable accounts only (ADR-0025). The harness-config page renders this
/// to show what's attached under each harness (issue #576).
#[command]
pub async fn get_provider_pairings() -> Result<Vec<ProviderPairing>, String> {
    Ok(preferences::effective_provider_pairings())
}

/// The **Model Providers** offered by "Add proxied provider" under `harness_id`,
/// surface-matched: only providers whose **Compatible API surface** that harness
/// speaks (issue #576). Empty for a native-only harness (Terminal, etc.).
#[command]
pub async fn compatible_providers_for_harness(
    harness_id: String,
) -> Result<Vec<ProviderAccount>, String> {
    Ok(preferences::compatible_providers_for_harness(&harness_id))
}

/// Attach a **Model Provider** to a harness over the harness's surface — the
/// "Add proxied provider" action (issue #576 / ADR-0025). Starts from
/// [`preferences::pairing_for`] defaults (first-class published endpoint, or a
/// surface-only shell for generics), then overlays optional `base_url` /
/// `model_tiers`. Requires a non-empty `base_url` after overlay (fill from
/// first-class or supply). `api_key`, when present, seeds the provider's
/// **global** key only if it has none (set-if-absent).
#[command]
pub async fn attach_proxied_provider(
    app: AppHandle,
    harness_id: String,
    provider_id: String,
    api_key: Option<String>,
    base_url: Option<String>,
    model_tiers: Option<ModelTiers>,
) -> Result<(), String> {
    let surface = preferences::harness_surface(&harness_id).ok_or_else(|| {
        format!("harness '{harness_id}' does not speak a proxy-capable surface")
    })?;
    let mut pairing = preferences::pairing_for(&harness_id, &provider_id).unwrap_or_else(|| {
        ProviderPairing {
            harness_id: harness_id.clone(),
            provider_id: provider_id.clone(),
            surface,
            base_url: None,
            model_tiers: ModelTiers::default(),
        }
    });
    // Surface-match gate: refuse when the provider doesn't expose this surface.
    let accounts = preferences::provider_accounts();
    let account = accounts.iter().find(|a| a.id == provider_id);
    // Keyed first-class may not be materialised yet — check catalog too.
    let surfaces = account
        .map(preferences::provider_surfaces)
        .or_else(|| {
            preferences::keyed_first_class_catalog()
                .into_iter()
                .find(|a| a.id == provider_id)
                .map(|a| preferences::provider_surfaces(&a))
        })
        .unwrap_or_default();
    if !surfaces.contains(&surface) {
        return Err(format!(
            "provider '{provider_id}' is not compatible with harness '{harness_id}'"
        ));
    }
    if let Some(url) = base_url.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        pairing.base_url = Some(url.to_string());
    }
    if let Some(tiers) = model_tiers {
        pairing.model_tiers = tiers;
    }
    if pairing.base_url.as_deref().is_none_or(|s| s.trim().is_empty()) {
        return Err(format!(
            "base_url is required to attach provider '{provider_id}' to harness '{harness_id}'"
        ));
    }
    let mut prefs = preferences::load()?;
    if let Some(key) = api_key.as_deref().filter(|k| !k.is_empty()) {
        preferences::set_account_key_if_absent(&mut prefs, &provider_id, key);
    }
    preferences::upsert_provider_pairing(&mut prefs, pairing);
    preferences::save(prefs)?;
    crate::services::usage::invalidate_cache();
    let _ = app.emit("provider-list-changed", ());
    Ok(())
}

/// Update `base_url` and/or `model_tiers` on an existing stored pairing
/// (ADR-0025 — Harnesses page inline edit). Errors if no pairing is stored for
/// the `(harness_id, provider_id)` key.
#[command]
pub async fn update_provider_pairing(
    app: AppHandle,
    harness_id: String,
    provider_id: String,
    base_url: Option<String>,
    model_tiers: Option<ModelTiers>,
) -> Result<(), String> {
    let mut prefs = preferences::load()?;
    let pairing = prefs
        .provider_pairings
        .iter_mut()
        .find(|p| p.harness_id == harness_id && p.provider_id == provider_id)
        .ok_or_else(|| {
            format!("no stored pairing for harness '{harness_id}' / provider '{provider_id}'")
        })?;
    if let Some(url) = base_url {
        let trimmed = url.trim();
        pairing.base_url = if trimmed.is_empty() { None } else { Some(trimmed.to_string()) };
    }
    if let Some(tiers) = model_tiers {
        pairing.model_tiers = tiers;
    }
    if pairing.base_url.as_deref().is_none_or(|s| s.trim().is_empty()) {
        return Err("base_url must be non-empty".to_string());
    }
    preferences::save(prefs)?;
    let _ = app.emit("provider-list-changed", ());
    Ok(())
}

/// Detach a stored **Proxied Provider** pairing (issue #576 / ADR-0025). Emits
/// `provider-list-changed` so the spawn menu drops the detached row.
#[command]
pub async fn remove_provider_pairing(
    app: AppHandle,
    harness_id: String,
    provider_id: String,
) -> Result<(), String> {
    let mut prefs = preferences::load()?;
    preferences::remove_provider_pairing(&mut prefs, &harness_id, &provider_id);
    preferences::save(prefs)?;
    let _ = app.emit("provider-list-changed", ());
    Ok(())
}
