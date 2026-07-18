//! Tauri commands for buildmesh-wide preferences.
//!
//! See `crate::preferences` for the persistence layer.

use crate::preferences::{self, AppPreferences, ProviderAccount, ProviderPairing};
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

/// The effective model-provider account list — code-defined built-ins with the
/// user's stored overrides merged in (issue #537). The settings UI renders this
/// rather than the raw `provider_accounts` override list so the built-ins are
/// always present without the frontend duplicating the default definitions.
#[command]
pub async fn get_provider_accounts() -> Result<Vec<ProviderAccount>, String> {
    Ok(preferences::provider_accounts())
}

/// Create or update a model-provider account (issue #537). For a custom
/// (non-built-in) account this also registers a paired harness profile so the
/// custom Claude-compatible provider appears in spawn menus — see
/// [`preferences::upsert_provider_account`]. Invalidates the usage cache so a
/// changed key/enabled-state is reflected on the next panel refresh.
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

/// Remove a stored provider account (and its paired custom harness profile, if
/// any). Removing a built-in just reverts it to the code-defined default.
/// Emits `provider-list-changed` for the same cross-component invalidation
/// reason as [`upsert_provider_account`].
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

/// The full effective set of **Proxied Provider** pairings — the derived default
/// Anthropic pairing for every keyed account plus the user's stored
/// cross-surface/cross-harness pairings. The harness-config page renders this to
/// show what's attached under each harness (issue #576).
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
/// "Add proxied provider" action (issue #576). The backend derives the pairing
/// (published first-class endpoint, or the Generic provider's declared one) so
/// the client never needs the surface→URL map; `api_key`, when present, seeds
/// the provider's **global** key only if it has none (set-if-absent). Rejects an
/// incompatible (harness, provider) pair — the same gate the picker enforces.
#[command]
pub async fn attach_proxied_provider(
    app: AppHandle,
    harness_id: String,
    provider_id: String,
    api_key: Option<String>,
) -> Result<(), String> {
    let pairing = preferences::pairing_for(&harness_id, &provider_id).ok_or_else(|| {
        format!("provider '{provider_id}' is not compatible with harness '{harness_id}'")
    })?;
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

/// Detach a stored **Proxied Provider** pairing (issue #576). A no-op for a
/// derived default pairing — the default Claude/Anthropic row is governed by the
/// account's enabled/keyed state on the Providers page, not here. Emits
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
