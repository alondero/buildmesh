//! Disk persistence, in-process cache, and atomic write coordination.
//!
//! This module is the **boundary** between the in-memory cache and durable
//! storage. Everything that touches `preferences.json` on disk lives here:
//! the `APP_DATA_DIR`/`CACHE`/`WRITE_LOCK` statics, the atomic temp-file
//! writer, and the small façade (`load`/`save`/`update`) that the rest of
//! the codebase uses.
//!
//! See the [module-level docs](super) for what concerns each submodule owns.

use super::migrations::migrate_prefs_json;
use super::model::AppPreferences;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

// Issue #1386: `APP_DATA_DIR` and `CACHE` are process-global in production
// (one app data dir per process) but per-TEST in tests, so concurrent
// `cargo test` runs don't collide on each other's state. The split is
// `cfg(test)`-gated so production binary size and behaviour is unchanged.

/// Set during Tauri `setup()` so callers don't need an `AppHandle`.
#[cfg(not(test))]
static APP_DATA_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// In-process cache, refreshed on every write. Reads consult the file only if
/// the cache is empty (first read).
#[cfg(not(test))]
static CACHE: Mutex<Option<AppPreferences>> = Mutex::new(None);

// Per-test-thread cell (issue #1386). Every `cargo test` worker thread
// gets its own `APP_DATA_DIR` and `CACHE` slot, so parallel tests each
// point at their own unique temp dir + private cache. Single-thread
// production keeps the global statics above.
#[cfg(test)]
thread_local! {
    static APP_DATA_DIR: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
    static CACHE: std::cell::RefCell<Option<AppPreferences>> =
        const { std::cell::RefCell::new(None) };
}

static WRITE_LOCK: Mutex<()> = Mutex::new(());

pub fn init(app_data_dir: PathBuf) {
    set_app_data_dir(Some(app_data_dir));
}

#[cfg(test)]
pub(crate) fn init_for_tests(app_data_dir: PathBuf) {
    init(app_data_dir);
    set_cache(None);
}

#[cfg(test)]
pub(crate) fn reset_for_tests() {
    set_app_data_dir(None);
    set_cache(None);
}

/// The app-data directory `init` was wired to, for sibling config files
/// that live next to `preferences.json` (e.g. Autopilot's `finish.md`,
/// issue #484). `None` before `init` runs (tests without a Tauri setup).
pub fn app_data_dir() -> Option<PathBuf> {
    read_app_data_dir()
}

#[cfg(test)]
fn set_app_data_dir(value: Option<PathBuf>) {
    APP_DATA_DIR.with(|d| *d.borrow_mut() = value);
}

#[cfg(not(test))]
fn set_app_data_dir(value: Option<PathBuf>) {
    *APP_DATA_DIR.lock().unwrap_or_else(|p| p.into_inner()) = value;
}

#[cfg(test)]
fn read_app_data_dir() -> Option<PathBuf> {
    APP_DATA_DIR.with(|d| d.borrow().clone())
}

#[cfg(not(test))]
fn read_app_data_dir() -> Option<PathBuf> {
    APP_DATA_DIR
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
}

#[cfg(test)]
fn set_cache(value: Option<AppPreferences>) {
    CACHE.with(|c| *c.borrow_mut() = value);
}

#[cfg(not(test))]
fn set_cache(value: Option<AppPreferences>) {
    *CACHE.lock().unwrap_or_else(|p| p.into_inner()) = value;
}

#[cfg(test)]
fn with_cache_mut<R>(f: impl FnOnce(&mut Option<AppPreferences>) -> R) -> R {
    CACHE.with(|c| f(&mut c.borrow_mut()))
}

#[cfg(not(test))]
fn with_cache_mut<R>(f: impl FnOnce(&mut Option<AppPreferences>) -> R) -> R {
    let mut g = CACHE.lock().unwrap_or_else(|p| p.into_inner());
    f(&mut g)
}

fn preferences_path() -> Result<PathBuf, String> {
    app_data_dir()
        .map(|d| d.join("preferences.json"))
        .ok_or_else(|| "preferences module not initialized".to_string())
}

pub(crate) fn read_from_disk() -> Result<AppPreferences, String> {
    let path = preferences_path()?;
    if !path.exists() {
        return Ok(AppPreferences::default());
    }
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("failed to read preferences.json: {}", e))?;
    // Tolerate malformed/empty files — preferences are non-critical.
    let mut value: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return Ok(AppPreferences::default()),
    };
    let changed = migrate_prefs_json(&mut value);
    // Round-trip the migrated JSON before persisting so a partially-unknown
    // payload (older field the Rust struct doesn't know about) doesn't get
    // overwritten by `AppPreferences::default()`. Issue #xxxx: silent
    // overwrite was a data-loss path.
    let prefs: AppPreferences = match serde_json::from_value(value.clone()) {
        Ok(p) => p,
        Err(_) => {
            tracing::warn!("preferences::read_from_disk post-migration deserialization failed; skipping persist");
            return Ok(AppPreferences::default());
        }
    };
    if changed {
        if let Err(e) = write_to_disk(&prefs) {
            tracing::warn!("preferences::read_from_disk migration save failed: {}", e);
        }
    }
    Ok(prefs)
}

pub(crate) fn write_to_disk(prefs: &AppPreferences) -> Result<(), String> {
    let _write_guard = WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = preferences_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create app data dir: {}", e))?;
    }
    let json = serde_json::to_string_pretty(prefs)
        .map_err(|e| format!("failed to serialize preferences: {}", e))?;
    let parent = path
        .parent()
        .ok_or_else(|| "preferences path has no parent directory".to_string())?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|e| format!("failed to create temporary preferences file: {e}"))?;
    temporary
        .write_all(json.as_bytes())
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|e| format!("failed to write temporary preferences file: {e}"))?;
    temporary
        .persist(&path)
        .map_err(|e| format!("failed to atomically replace preferences.json: {}", e.error))?;
    Ok(())
}

/// Load preferences, populating the in-process cache on first call.
///
/// Recovers from a poisoned `CACHE` mutex instead of panicking (issue
/// #1224). The cache is a plain `Mutex<Option<AppPreferences>>` —
/// a panic in a previous holder would have released the guard on
/// unwind, leaving the inner value in a consistent (None-or-fully-
/// populated) state. `.unwrap()` on `PoisonError` would brick every
/// subsequent `load`/`save` and freeze the whole preferences surface;
/// `into_inner()` lets the next caller decide whether to refresh
/// from disk.
///
/// Issue #1386: in tests the cache lives in a `thread_local!` `RefCell`
/// (one slot per `cargo test` worker thread) so parallel tests don't
/// collide on the same in-memory value. Production keeps a process-
/// global `Mutex` — there's exactly one app data dir per process, so
/// global state is correct.
pub fn load() -> Result<AppPreferences, String> {
    // Cold-cache populate, mutator, and cache publish all happen under the
    // mutex — the same contract as the pre-issue-#1386 implementation,
    // routed through the `with_cache_mut` cfg-divergent helper. The
    // closure captures the result so we don't have to thread `?` through
    // helper returns.
    let result: Result<AppPreferences, String> = with_cache_mut(|guard| {
        if let Some(cached) = guard.as_ref() {
            return Ok(cached.clone());
        }
        let prefs = read_from_disk()?;
        *guard = Some(prefs.clone());
        Ok(prefs)
    });
    result
}

/// Persist preferences to disk and refresh the cache.
pub fn save(prefs: AppPreferences) -> Result<(), String> {
    write_to_disk(&prefs)?;
    set_cache(Some(prefs));
    Ok(())
}

/// Atomically mutate the latest cached preference value and persist it while
/// serialising competing read-modify-write operations.
///
/// The mutex is held across the cold-cache populate, the mutator, the disk
/// write, and the publish — same semantic as the pre-issue-#1386
/// implementation, just routed through the `with_cache_mut` cfg-divergent
/// helper. The mutex is also the "serialising competing RMWs" gate the
/// docstring promises; releasing it between mutator and write would let two
/// concurrent updaters both win the in-memory race against the on-disk one.
pub fn update(mutator: impl FnOnce(&mut AppPreferences)) -> Result<AppPreferences, String> {
    let result: Result<AppPreferences, String> = with_cache_mut(|guard| {
        if guard.is_none() {
            *guard = Some(read_from_disk()?);
        }
        let mut candidate = guard
            .as_ref()
            .expect("preferences cache was initialized")
            .clone();
        mutator(&mut candidate);
        // Publish the new cached value only after the durable atomic
        // replacement succeeds — the disk I/O is serialised by the
        // outer-mutex hold AND by `WRITE_LOCK` inside `write_to_disk`. A
        // failed write must not manufacture an in-memory verification
        // record that launch preflight could mistake for persisted proof.
        write_to_disk(&candidate)?;
        *guard = Some(candidate.clone());
        Ok(candidate)
    });
    result
}

/// Convenience: returns the app-wide default provider id, if any.
/// Empty strings are treated as `None` to match how the per-mesh column
/// is normalized elsewhere (see `commands::mesh::get_default_provider`).
///
/// A load failure (e.g. preferences module not initialised, or unreadable
/// file) is logged once and treated as "no override". We don't propagate
/// the error because the precedence chain has a hardcoded fallback — but
/// without the warn! a misconfigured environment would silently ignore the
/// user's setting with no trace.
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
/// `.claude/worktrees` under the Mesh root, overridden per-Mesh).
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
/// autopilot nodes across every mesh (see [`AppPreferences::autopilot_pool_size`]).
/// `None` means "no global cap" — the per-mesh `autopilot_concurrency_limit`
/// values are the only gate, which was the behaviour before this setting
/// existed. A load failure is logged and treated as "no cap" for the same
/// reason as [`default_provider`]: the poller must keep working even when
/// preferences are unreadable.
pub fn autopilot_pool_size() -> Option<u32> {
    match load() {
        Ok(prefs) => prefs.autopilot_pool_size,
        Err(e) => {
            tracing::warn!(
                "preferences::autopilot_pool_size load failed, treating as uncapped: {}",
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
