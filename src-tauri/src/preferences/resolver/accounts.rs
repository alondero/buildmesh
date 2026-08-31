//! Account list — effective-account resolution, key lookups, mutators.

use super::super::model::{AppPreferences, ProviderAccount};
use super::super::storage::{load, save};
use super::catalog::{
    default_provider_accounts, is_claude_compatible_id, keyed_first_class_template,
};

/// The effective account list: code-defined defaults with the user's stored
/// `provider_accounts` merged over them by `id` (user wins / new ids append).
/// Mirrors [`super::harness::harness_profiles`] so a built-in can never be
/// removed by an empty or partial `preferences.json`.
pub fn provider_accounts() -> Vec<ProviderAccount> {
    let mut prefs = match load() {
        Ok(prefs) => prefs,
        Err(e) => {
            tracing::warn!("preferences::provider_accounts load failed, using defaults: {}", e);
            return default_provider_accounts();
        }
    };
    let merged = merge_provider_accounts(default_provider_accounts(), prefs.provider_accounts.clone());
    let (migrated, changed) = migrate_kimi_companion(merged);
    // One-shot persistence: if a PR #1044 `kimi-via-claude` row was carried
    // over to the first-class `kimi` row, write the cleaned list back so the
    // stale row stops haunting subsequent reads. Subsequent reads see no
    // companion, so `changed` is false and the save is skipped.
    if changed {
        prefs.provider_accounts = migrated.clone();
        if let Err(e) = save(prefs) {
            tracing::warn!("preferences::provider_accounts migration save failed: {}", e);
        }
    }
    migrated
}

/// Migrate any stored `kimi-via-claude` companion (left over from PR #1044)
/// into the first-class `kimi` row and drop the companion. **Key only**
/// (ADR-0025 — endpoint fields live on pairings; JSON migration handles
/// legacy endpoint → pairing before this runs). Returns the migrated list
/// plus a `changed` flag so the caller can persist the result when the
/// migration actually moved state. Pure — no disk side effects.
fn migrate_kimi_companion(
    mut accounts: Vec<ProviderAccount>,
) -> (Vec<ProviderAccount>, bool) {
    let Some(companion_idx) = accounts.iter().position(|a| a.id == "kimi-via-claude") else {
        return (accounts, false);
    };
    let companion = accounts[companion_idx].clone();
    if let Some(kimi) = accounts.iter_mut().find(|a| a.id == "kimi") {
        if kimi.api_key.as_ref().is_none_or(|v| v.is_empty()) {
            kimi.api_key = companion.api_key.clone();
        }
    } else if let Some(mut kimi) = keyed_first_class_template("kimi") {
        kimi.api_key = companion.api_key.clone();
        if !companion.enabled {
            kimi.enabled = false;
        }
        accounts.push(kimi);
    }
    accounts.remove(companion_idx);
    (accounts, true)
}

/// Pure merge — defaults first, then each stored account overrides by `id` or
/// appends. Split out from [`provider_accounts`] so it's unit-testable without disk.
///
/// `claude_compatible` is **re-derived from the id** on every account afterwards,
/// so it's always correct regardless of what was stored (an older
/// `preferences.json` predating the field, or a stale value sent from the UI).
fn merge_provider_accounts(
    mut accounts: Vec<ProviderAccount>,
    stored: Vec<ProviderAccount>,
) -> Vec<ProviderAccount> {
    for account in stored {
        if let Some(existing) = accounts.iter_mut().find(|a| a.id == account.id) {
            *existing = account;
        } else {
            accounts.push(account);
        }
    }
    for account in accounts.iter_mut() {
        account.claude_compatible = is_claude_compatible_id(&account.id);
    }
    accounts
}

/// Resolve the effective MiniMax API key: the minimax account's `api_key`, then
/// the legacy flat `minimax_api_key` field (read-through so a key stored before
/// #537 isn't lost). Empty strings are treated as absent. A single `load()` feeds
/// both layers so the result is a consistent snapshot even under a concurrent save.
pub fn minimax_api_key_resolved() -> Option<String> {
    let non_empty = |s: Option<String>| s.filter(|v| !v.is_empty());
    let prefs = match load() {
        Ok(prefs) => prefs,
        Err(_) => return None,
    };
    let from_account = merge_provider_accounts(default_provider_accounts(), prefs.provider_accounts.clone())
        .into_iter()
        .find(|a| a.id == "minimax")
        .and_then(|a| a.api_key);
    non_empty(from_account).or_else(|| non_empty(prefs.minimax_api_key))
}

/// Resolve the effective Kimi API key from the merged provider-accounts list.
/// Kimi has no legacy flat field (unlike MiniMax's `minimax_api_key`) so this
/// is a straight lookup, but lives here as the single seam so a future legacy
/// fallback (e.g. a pre-config.json migration) can be added in one place
/// without touching `commands::usage::cached_or_fetch`. Empty strings are
/// treated as absent.
pub fn kimi_api_key_resolved() -> Option<String> {
    merge_provider_accounts(default_provider_accounts(), load().ok()?.provider_accounts)
        .into_iter()
        .find(|a| a.id == "kimi")
        .and_then(|a| a.api_key)
        .filter(|v| !v.is_empty())
}

/// Resolve the effective OpenRouter API key from the merged provider-accounts
/// list. Brand-new id (post-#570 land) — no legacy flat field, identical
/// lookup shape to [`kimi_api_key_resolved`] but kept as a separate symbol so
/// each provider's single seam stays explicit.
pub fn openrouter_api_key_resolved() -> Option<String> {
    merge_provider_accounts(default_provider_accounts(), load().ok()?.provider_accounts)
        .into_iter()
        .find(|a| a.id == "openrouter")
        .and_then(|a| a.api_key)
        .filter(|v| !v.is_empty())
}

/// Resolve the effective OpenAI Platform API key from the merged provider-
/// accounts list (issue #1109, ADR-0026). New keyed id — no legacy flat
/// field; identical lookup shape to [`kimi_api_key_resolved`] but kept
/// separate so the future legacy-fallback seam stays one symbol per
/// provider. Empty strings collapse to `None` so a half-cleared config
/// doesn't surface as a logged-out row.
pub fn openai_api_key_resolved() -> Option<String> {
    merge_provider_accounts(default_provider_accounts(), load().ok()?.provider_accounts)
        .into_iter()
        .find(|a| a.id == "openai")
        .and_then(|a| a.api_key)
        .filter(|v| !v.is_empty())
}

/// Resolve the effective DeepSeek API key from the merged provider-accounts
/// list (issue #1127). Brand-new keyed id — no legacy flat field; identical
/// lookup shape to [`openai_api_key_resolved`] so each provider's single seam
/// stays explicit. Empty strings collapse to `None` so a half-cleared config
/// doesn't surface as a logged-out row in the Usage panel.
pub fn deepseek_api_key_resolved() -> Option<String> {
    merge_provider_accounts(default_provider_accounts(), load().ok()?.provider_accounts)
        .into_iter()
        .find(|a| a.id == "deepseek")
        .and_then(|a| a.api_key)
        .filter(|v| !v.is_empty())
}

/// Upsert a provider account into `prefs` (by `id`). Pure: mutates the passed
/// `prefs` so the command layer stays a thin load→mutate→save.
///
/// No longer materializes a paired [`super::super::model::HarnessProfile`]: the spawn menu is now
/// *derived* from the accounts list (see `agent::provider_menu::compose_provider_menu`),
/// so an enabled, keyed, Claude-compatible account — built-in MiniMax or a
/// custom endpoint alike — appears automatically, and clearing its key or
/// disabling it removes it with no second list to keep in sync (issue #568).
pub fn upsert_provider_account(prefs: &mut AppPreferences, account: ProviderAccount) {
    if let Some(existing) = prefs.provider_accounts.iter_mut().find(|a| a.id == account.id) {
        *existing = account;
    } else {
        prefs.provider_accounts.push(account);
    }
}

/// Remove a stored provider account by `id`. Built-in defaults can't truly be
/// deleted — removing a built-in's stored override just reverts it to the code
/// default (which carries no key, so it drops out of the derived spawn menu).
pub fn remove_provider_account(prefs: &mut AppPreferences, id: &str) {
    prefs.provider_accounts.retain(|a| a.id != id);
}

/// Set a provider account's **global** API key only if it currently has none
/// (the "set-if-absent from the attach flow" rule, ADR-0016 §4 / issue #576).
/// Returns whether a key was written. The canonical key editor stays on the
/// Providers page; this lets the harness-config attach flow seed a key for a
/// provider the user hasn't configured yet without ever *overwriting* one.
///
/// Looks up the effective account (defaults + stored). For a keyed first-class
/// id not yet materialised, seeds from [`super::catalog::keyed_first_class_catalog`].
pub fn set_account_key_if_absent(prefs: &mut AppPreferences, provider_id: &str, key: &str) -> bool {
    if key.is_empty() {
        return false;
    }
    let effective =
        merge_provider_accounts(default_provider_accounts(), prefs.provider_accounts.clone());
    let Some(account) = effective
        .into_iter()
        .find(|a| a.id == provider_id)
        .or_else(|| keyed_first_class_template(provider_id))
    else {
        return false;
    };
    if account.api_key.as_deref().is_some_and(|k| !k.is_empty()) {
        return false; // already keyed — never overwrite
    }
    upsert_provider_account(
        prefs,
        ProviderAccount {
            api_key: Some(key.to_string()),
            ..account
        },
    );
    true
}