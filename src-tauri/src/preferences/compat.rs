//! The default profile and the free functions over it.
//!
//! Issue #1482 keeps one production composition root: [`init`] installs the
//! single [`PreferencesStore`] that Tauri's `setup()` wires to the app-data
//! directory, and the free functions below are the compatibility layer for the
//! call sites that cannot yet take a handle. They delegate straight to that
//! store — there is no second cache, lock or revision implementation here.
//!
//! # What stays test-only
//!
//! Selection only. `cargo test` runs suites in parallel and each needs its own
//! temporary profile, so instrumented builds keep the installed store in a
//! per-worker-thread slot (issue #1386) instead of the process-wide one. Both
//! slots are the same `Mutex<Option<PreferencesStore>>`, written and read by
//! the same functions; the store, its cache mutex, its write lock and its
//! revision sequence are identical either way. *Which* profile these functions
//! address is the only difference, and storage semantics are not forked.
//!
//! This selection disappears with the last free-function caller (issue #1482,
//! migration slices 3–4). Suites that already hold handles
//! (`preferences::tests::store_tests`) never touch it.

use super::model::AppPreferences;
use super::recovery::{PreferencesHealth, RecoveryOutcome};
#[cfg(test)]
use super::storage::LoadState;
use super::store::{self, PreferencesStore, StoreSnapshot};
use std::path::PathBuf;
use std::sync::Mutex;

/// The installed default profile. One `Mutex<Option<PreferencesStore>>` — the
/// same type, read and written by the same two functions, in every build.
///
/// Production keeps the slot process-wide (issue #1482's single composition
/// root). Test builds keep one slot **per worker thread** so parallel suites
/// each address their own temporary profile (issue #1386) — the scope of the
/// slot is the only difference, and it is selection, not storage semantics:
/// the store's own cache mutex, write lock and revision are the shared
/// implementation below.
#[cfg(not(test))]
static DEFAULT_STORE: Mutex<Option<PreferencesStore>> = Mutex::new(None);

#[cfg(test)]
thread_local! {
    static DEFAULT_STORE: Mutex<Option<PreferencesStore>> = const { Mutex::new(None) };
}

/// Read the installed store out of a slot. Production passes its static;
/// tests pass their thread's.
fn clone_installed(slot: &Mutex<Option<PreferencesStore>>) -> Option<PreferencesStore> {
    slot.lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

fn install_into(slot: &Mutex<Option<PreferencesStore>>, store: Option<PreferencesStore>) {
    *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = store;
}

fn current_store() -> Result<PreferencesStore, String> {
    #[cfg(not(test))]
    let current = clone_installed(&DEFAULT_STORE);
    #[cfg(test)]
    let current = DEFAULT_STORE.with(clone_installed);
    current.ok_or_else(|| "preferences module not initialized".to_string())
}

fn install_store(store: Option<PreferencesStore>) {
    #[cfg(not(test))]
    install_into(&DEFAULT_STORE, store);
    #[cfg(test)]
    DEFAULT_STORE.with(|slot| install_into(slot, store));
}

/// Install the process-wide profile. Called once from Tauri `setup()`.
pub fn init(app_data_dir: PathBuf) {
    install_store(Some(PreferencesStore::new(app_data_dir)));
}

#[cfg(test)]
pub(crate) fn init_for_tests(app_data_dir: PathBuf) {
    // A swapped app-data dir is a changed preference source. A fresh store
    // carries a fresh (globally advancing) revision, so derived caches keyed
    // on the generation cannot serve values computed against the previous one.
    install_store(Some(PreferencesStore::new(app_data_dir)));
}

#[cfg(test)]
pub(crate) fn reset_for_tests() {
    install_store(None);
    super::secrets::test_support::reset();
}

/// The app-data directory `init` was wired to, for sibling config files
/// that live next to `preferences.json` (e.g. Autopilot's `finish.md`,
/// issue #484). `None` before `init` runs (tests without a Tauri setup).
pub fn app_data_dir() -> Option<PathBuf> {
    current_store().ok().map(|store| store.dir().to_path_buf())
}

pub(crate) fn preferences_path() -> Result<PathBuf, String> {
    Ok(current_store()?.preferences_path())
}

/// The default store's current revision. See [`PreferencesStore::revision`].
///
/// With no store installed this returns the last revision the process
/// allocated, so the value never moves backwards when a test replaces its
/// default profile.
pub fn generation() -> u64 {
    match current_store() {
        Ok(store) => store.revision(),
        Err(_) => store::last_revision(),
    }
}

/// The default store's source identity, or `0` when none is installed.
pub fn source() -> u64 {
    current_store().map(|store| store.source()).unwrap_or(0)
}

/// The default store's `(source, revision)` pair, or the process fallback when
/// no store is installed.
///
/// Derived caches take both halves from one call so the pair is sampled from
/// the same handle, not from two lookups that a `init`/`reset` could straddle.
pub fn identity() -> (u64, u64) {
    match current_store() {
        Ok(store) => store.identity(),
        Err(_) => (0, store::last_revision()),
    }
}

// Counts preference reads on this thread so a test can assert that a
// derivation reads preferences once (issue #2532).
//
// Per-thread, like the test default store (issue #1386): a parallel test on
// another worker thread must not change this thread's count.
#[cfg(test)]
// allow-test-in-production: the per-thread load counter lets a sibling suite assert that a
// derivation reads preferences once.
thread_local! {
    static LOAD_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
// allow-test-in-production: the counter's writer, called from the one read path it counts.
fn count_load() {
    LOAD_COUNT.with(|count| count.set(count.get() + 1));
}

/// Test-only read of the raw on-disk state, without touching the cache.
///
/// Kept on the compatibility layer because the recovery suites drive their
/// scenarios through the default profile's files.
#[cfg(test)]
pub(crate) fn read_state() -> Result<LoadState, String> {
    current_store()?.read_state()
}

/// The number of [`snapshot`] / [`load`] calls made on this thread.
#[cfg(test)]
// allow-test-in-production: the counter's reader, called from a sibling suite's assertion.
pub(crate) fn load_count() -> usize {
    LOAD_COUNT.with(|count| count.get())
}

/// Read the default profile's value, source and revision.
///
/// This is the one read path: [`load`] and the resolver's
/// [`super::resolver::load_snapshot`] both come through here, so the "one
/// preferences read" guarantee holds for the value *and* the revision.
pub fn snapshot() -> Result<StoreSnapshot, String> {
    #[cfg(test)]
    // allow-test-in-production: the counter's writer, called from the one read path it counts.
    count_load();
    current_store()?.snapshot()
}

/// Load preferences, populating the in-process cache on first call.
///
/// Recovers from a poisoned cache mutex instead of panicking (issue #1224).
/// The cache is a plain `Mutex<Option<AppPreferences>>` — a panic in a
/// previous holder would have released the guard on unwind, leaving the inner
/// value in a consistent (None-or-fully-populated) state. `.unwrap()` on
/// `PoisonError` would brick every subsequent `load`/`save` and freeze the
/// whole preferences surface; `into_inner()` lets the next caller decide
/// whether to refresh from disk.
///
/// Issue #1523: a **corrupt** file still yields defaults here, so every
/// read-only caller (spawn routing, the circuit classifier, the usage
/// panel) keeps working instead of failing closed on a file the app can
/// still show. Those defaults are deliberately *not* published to the
/// cache: the write path re-reads the file and refuses, so no ordinary
/// settings change can turn "unreadable" into "overwritten".
pub fn load() -> Result<AppPreferences, String> {
    #[cfg(test)]
    // allow-test-in-production: the counter's writer, called from the one read path it counts.
    count_load();
    current_store()?.load()
}

/// Persist preferences to disk and refresh the cache.
pub fn save(prefs: AppPreferences) -> Result<(), String> {
    current_store()?.save(prefs).map(|_| ())
}

/// Atomically mutate the latest cached preference value and persist it while
/// serialising competing read-modify-write operations.
pub fn update(mutator: impl FnOnce(&mut AppPreferences)) -> Result<AppPreferences, String> {
    Ok(current_store()?.update(mutator)?.preferences)
}

pub(crate) fn try_update(
    mutator: impl FnOnce(&mut AppPreferences) -> Result<(), String>,
) -> Result<AppPreferences, String> {
    Ok(current_store()?.try_update(mutator)?.preferences)
}

/// Current health of the on-disk file, for the UI's recovery surface.
pub fn health() -> Result<PreferencesHealth, String> {
    current_store()?.health()
}

/// Explicit recovery: put the last-known-good backup back, archiving
/// whatever is on disk first.
pub fn restore_backup() -> Result<RecoveryOutcome, String> {
    current_store()?.restore_backup()
}

/// Explicit recovery: archive the existing file and start from defaults.
pub fn reset() -> Result<RecoveryOutcome, String> {
    current_store()?.reset()
}

/// The corrupt `preferences.json`'s location, for the "open file location"
/// action. Returns the containing directory, because a file manager opening
/// a single file renders it with whichever app claims the extension — a JSON
/// editor, not Explorer.
pub fn preferences_directory() -> Result<PathBuf, String> {
    Ok(current_store()?.dir().to_path_buf())
}

/// Convenience: returns the app-wide default provider id, if any.
/// Empty strings are treated as `None` to match how the per-mesh column
/// is normalized elsewhere (see `commands::mesh::get_default_provider`).
///
/// A load failure (e.g. preferences module not initialised, or unreadable
/// file) is logged once and treated as "no override". We don't propagate
/// the error because the precedence chain has a hardcoded fallback — but
/// without the warn! a misconfigured environment would silently ignore
/// the user's setting with no trace.
pub fn default_provider() -> Option<String> {
    match load() {
        Ok(prefs) => prefs.default_provider.filter(|s| !s.is_empty()),
        Err(e) => {
            tracing::warn!(
                "preferences::default_provider load failed, falling back: {}",
                e
            );
            None
        }
    }
}

/// The optional app-wide reviewer Spawn Option. A blank value means reviews
/// inherit the source/parent provider, while a configured value lets an
/// adversarial review use an independent harness.
pub fn reviewer_provider() -> Option<String> {
    match load() {
        Ok(prefs) => prefs
            .reviewer_provider
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        Err(e) => {
            tracing::warn!(
                "preferences::reviewer_provider load failed, falling back: {}",
                e
            );
            None
        }
    }
}

/// The user-configured backend the session-naming helper uses to summarise
/// PTY output into a slug (issue #824). `None` means "auto-naming is off" —
/// the session_naming module short-circuits and nodes retain their random
/// `adjective-adjective-noun` slug. An empty string is normalised to `None`
/// here so a save with `""` from the frontend acts the same as a clear.
///
/// Distinct from `default_provider`: the naming helper runs frequently on
/// content that is well below the front-line model's intelligence
/// threshold, so the user explicitly opts in via Settings rather than
/// inheriting whatever provider a spawned node happens to be on (which can
/// be an expensive tier like Opus with xhigh effort).
///
/// The naming helper treats this as a *spawn-option id* (e.g. `"minimax"`,
/// `"claude:minimax"`, `"claude:openrouter"`) and resolves it through
/// [`crate::preferences::resolve_provider_env`] the same way node spawns
/// do — so a user who already configured a Provider Account can reuse it
/// here for free. Built-in Anthropic (`"anthropic"`) is special-cased
/// inside `session_naming` to pin a cheap haiku tier; the historical
/// `minimax_backend_env()` side-channel is no longer the implicit
/// default.
pub fn naming_provider() -> Option<String> {
    match load() {
        Ok(prefs) => prefs.naming_provider.filter(|s| !s.is_empty()),
        Err(e) => {
            tracing::warn!(
                "preferences::naming_provider load failed, falling back: {}",
                e
            );
            None
        }
    }
}

/// Buildmesh-wide default Worktree Node directory (issue #1519).
/// Trimmed raw input; blank collapses to `None` (default
/// `.worktrees` under the Mesh root, overridden per-Mesh).
/// A load failure logs and falls back to `None` like [`default_provider`].
pub fn worktree_directory() -> Option<String> {
    match load() {
        Ok(prefs) => prefs
            .worktree_directory
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        Err(e) => {
            tracing::warn!(
                "preferences::worktree_directory load failed, falling back: {}",
                e
            );
            None
        }
    }
}

/// The global autopilot pool size — the app-wide cap on concurrently active
/// autopilot nodes across every mesh (see [`AppPreferences::circuit_agent_pool_size`]).
/// `None` means "no global cap" — the per-mesh `autopilot_concurrency_limit`
/// values are the only gate, which was the behaviour before this setting
/// existed. A load failure is logged and treated as "no cap" for the same
/// reason as [`default_provider`]: the poller must keep working even when
/// preferences are unreadable.
pub fn circuit_agent_pool_size() -> Option<u32> {
    match load() {
        Ok(prefs) => prefs.circuit_agent_pool_size,
        Err(e) => {
            tracing::warn!(
                "preferences::circuit_agent_pool_size load failed, treating as uncapped: {}",
                e
            );
            None
        }
    }
}

/// Custom template for the initial prompt of agents spawned from the
/// Probe's GitHub Issues tab. `None` means "use the built-in wording".
/// A blank stored value collapses to `None` so clearing the Settings
/// field restores the default. A load failure logs and falls back to
/// `None` like [`default_provider`].
pub fn issue_spawn_prompt() -> Option<String> {
    match load() {
        Ok(prefs) => prefs
            .issue_spawn_prompt
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        Err(e) => {
            tracing::warn!(
                "preferences::issue_spawn_prompt load failed, falling back to default: {}",
                e
            );
            None
        }
    }
}

/// Custom template for the initial prompt of agents spawned from the
/// Probe's Pull Requests tab. Same `None`/blank/load-failure semantics
/// as [`issue_spawn_prompt`].
pub fn pr_spawn_prompt() -> Option<String> {
    match load() {
        Ok(prefs) => prefs
            .pr_spawn_prompt
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        Err(e) => {
            tracing::warn!(
                "preferences::pr_spawn_prompt load failed, falling back to default: {}",
                e
            );
            None
        }
    }
}

/// One-shot normalization of legacy bare `default_provider` values to
/// the post-#575 composite form (`minimax` → `claude:minimax`).
///
/// The v19 Spawn Option composite-id migration rewrote
/// `agent_nodes.provider` but never touched `preferences.json::default_provider`
/// (issue #575 / ADR-0016 §6). A user whose app-wide default was set
/// before #575 lands keeps the legacy bare form in their preferences.json
/// — and the bare form routes through `resolve_provider_env` to the keyed
/// **account** instead of the post-#575 proxied pairing, which silently
/// spawns Claude-CLI sessions against the wrong endpoint.
///
/// `kimi` is intentionally absent post-#918: bare `"kimi"` now resolves
/// to the native Kimi Code harness via `Provider::from_db_str`, so a
/// legacy bare `kimi` preference reads through to `Provider::Kimi`
/// directly without a rewrite. Rewriting to `claude:kimi` would put the
/// user in a state with no Proxied row in the spawn menu (Kimi Code is
/// self_auth, not Claude-compatible).
///
/// Called from `lib.rs::setup` immediately after `preferences::init`,
/// so this can `load()` against the real on-disk file. Idempotent —
/// already-composite values, native harness ids, and `None` are left
/// alone. On a no-op (the common case after the first launch) this is a
/// single cached read plus an equality check.
pub(crate) fn ensure_default_provider_normalized() -> Result<(), String> {
    let mut prefs = load()?;
    let normalized = prefs
        .default_provider
        .as_deref()
        .and_then(normalize_legacy_default_provider);
    if let Some(new_value) = normalized {
        tracing::info!(
            "default_provider normalized: {} → {}",
            prefs.default_provider.as_deref().unwrap_or(""),
            new_value
        );
        prefs.default_provider = Some(new_value.to_string());
        save(prefs)?;
    }
    Ok(())
}

/// Pure translation table for [`ensure_default_provider_normalized`].
/// Kept separate so it's the single seam to extend if a future legacy
/// bare id lands (every addition is one match arm + one unit test).
fn normalize_legacy_default_provider(bare: &str) -> Option<&'static str> {
    match bare {
        "minimax" => Some("claude:minimax"),
        // `kimi` removed post-#918 (Kimi Code is a native harness, not a
        // Claude-compatible Proxied row). See the fn docstring.
        _ => None,
    }
}

#[cfg(test)]
mod generation_tests {
    use super::*;

    /// Issue #1752: both writers must advance the generation, because the
    /// launch-routing cache treats "generation changed" as its invalidation
    /// signal. A `save` or `update` that forgot to bump would let a stale
    /// routing (endpoint / model / credential) survive a settings change.
    ///
    /// Asserted as strict monotonicity rather than an exact delta — the
    /// counter is process-global and other (parallel) tests may also write.
    #[test]
    fn both_writers_advance_the_generation() {
        let dir = tempfile::TempDir::new().unwrap();
        init_for_tests(dir.path().to_path_buf());

        let before_save = generation();
        save(AppPreferences::default()).expect("save");
        let after_save = generation();
        assert!(
            after_save > before_save,
            "save must bump the generation ({before_save} -> {after_save})"
        );

        update(|prefs| prefs.default_provider = Some("terminal".into())).expect("update");
        let after_update = generation();
        assert!(
            after_update > after_save,
            "update must bump the generation ({after_save} -> {after_update})"
        );

        reset_for_tests();
    }
}
