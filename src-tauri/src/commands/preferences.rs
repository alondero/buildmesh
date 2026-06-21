//! Tauri commands for buildmesh-wide preferences.
//!
//! See `crate::preferences` for the persistence layer.

use crate::preferences::{self, AppPreferences, ProviderAccount};
use tauri::{command, AppHandle, Emitter};

/// Read the persisted buildmesh-wide preferences. Always returns a value —
/// a missing or malformed file yields `AppPreferences::default()`.
#[command]
pub async fn get_app_preferences() -> Result<AppPreferences, String> {
    preferences::load()
}

/// Set the buildmesh-wide default provider. Pass `None` (or an empty string,
/// which is normalised away) to clear the override and restore the hardcoded
/// `anthropic` fallback.
#[command]
pub async fn set_app_default_provider(provider: Option<String>) -> Result<(), String> {
    let mut prefs = preferences::load()?;
    prefs.default_provider = provider.filter(|s| !s.is_empty());
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
