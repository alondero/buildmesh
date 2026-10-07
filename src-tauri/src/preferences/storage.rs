//! Disk persistence, in-process cache, and atomic write coordination.
//!
//! This module is the **boundary** between the in-memory cache and durable
//! storage. It owns the `APP_DATA_DIR`/`CACHE`/`WRITE_LOCK` statics, the
//! `load`/`save`/`update` façade the rest of the codebase uses, and — the
//! reason this module exists at all — the rule that decides what a
//! caller is *allowed* to do with a file that is not readable
//! ([`LoadState`], issue #1523).
//!
//! The bytes on disk are owned by [`super::recovery`]: it classifies a
//! payload, keeps the last-known-good backup, and performs the explicit
//! recovery actions. This module owns the *process* state — the cache, the
//! generation counter, and the write lock — and delegates every byte to
//! `recovery`. The split is deliberate: `recovery::classify` is pure, so
//! the classification rules are testable without a filesystem, while the
//! stateful decision ("may this write land?") has exactly one home.
//!
//! See the [module-level docs](super) for what concerns each submodule owns.

use super::model::AppPreferences;
use super::recovery::{self, CorruptionInfo, PreferencesHealth, RecoveryOutcome};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
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

/// Monotonic counter bumped on every successful preference write (issue #1752).
/// Derived caches outside this module (e.g. the spawn launch-routing cache in
/// `agent::launch_routing`) snapshot this value and treat a change as "my
/// cached value may be stale — drop it". Both writers — [`save`] and
/// [`update`] — bump it, and every mutation path in the tree funnels through
/// those two, so no settings change can slip past without invalidating.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// The current preference generation. See [`GENERATION`].
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

/// Bump the generation after a durable write has landed.
fn bump_generation() {
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

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
    super::secrets::test_support::reset();
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

/// The outcome of reading `preferences.json` off disk.
///
/// Issue #1523 made this a typed result because the *unreadable* case used
/// to be indistinguishable from "no file yet": both produced
/// `AppPreferences::default()`, that default was published as the
/// authoritative cache, and the next ordinary settings change replaced the
/// user's file with it. Only [`LoadState::Missing`] may seed the cache with
/// defaults — a corrupt read is served defaults in memory (so read-only
/// callers keep working) but is never published and never written back.
#[derive(Debug, Clone)]
pub enum LoadState {
    /// No file on disk yet — a fresh install. The only state that may
    /// populate the writable cache with defaults.
    Missing,
    /// The file parsed, migrated, and deserialized cleanly.
    Healthy(Box<AppPreferences>),
    /// The file exists but could not be turned into [`AppPreferences`]. The
    /// original bytes are untouched on disk; [`CorruptionInfo`] says why.
    Corrupt(Box<CorruptionInfo>),
}

/// The reconciled default preferences a fresh install starts from.
fn defaults() -> AppPreferences {
    let mut prefs = AppPreferences::default();
    super::launch_configurations::reconcile(&mut prefs);
    prefs
}

/// Read and classify the on-disk file. Never writes, never mutates the cache.
///
/// Every write path calls this first and refuses on anything but
/// [`LoadState::Healthy`] / [`LoadState::Missing`] — re-reading rather than
/// latching a flag from an earlier load, because the invariant has to hold
/// for a process that writes before it ever reads, and for a cache that was
/// populated before the file went bad underneath us. A `preferences.json` is
/// a few tens of KB, so one extra read per write is cheaper than a latch that
/// can disagree with the disk.
///
/// A read **error** refuses the write too, even though it proves nothing about
/// the file's *contents*. A file we cannot read may be perfectly valid, and
/// `fs::rename` can replace a file whose read access is denied — so
/// proceeding would overwrite unreadable user data with defaults, which is
/// the exact loss #1523 exists to prevent. Not being able to read the file is
/// itself a reason to stop and say so.
pub(crate) fn read_state() -> Result<LoadState, String> {
    read_state_flagging_plaintext().map(|(state, _)| state)
}

/// [`read_state`] plus whether the file still held a plaintext API key
/// (issue #830) — the cue for [`load`] to [`scrub_plaintext_keys`].
fn read_state_flagging_plaintext() -> Result<(LoadState, bool), String> {
    let path = preferences_path()?;
    let raw = match std::fs::read(&path) {
        Ok(raw) => raw,
        // `NotADirectory` counts as "no file" alongside `NotFound`: a path
        // component that is not a directory means there is no
        // preferences.json and never was — unlike a permission or I/O error,
        // which says nothing about the file's existence. Without this the
        // classification is platform-dependent, because Windows reports the
        // same shape as ERROR_PATH_NOT_FOUND (`NotFound`) while Linux reports
        // ENOTDIR.
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok((LoadState::Missing, false))
        }
        Err(e) => return Err(format!("failed to read preferences.json: {}", e)),
    };
    // Keys are restored into the JSON before the migrations and before
    // `reconcile` (launch availability is derived from whether an account has
    // one), so everything downstream sees exactly what a plaintext file gave.
    let mut plaintext_in_file = false;
    match recovery::classify_with(&raw, |value| {
        plaintext_in_file = super::secrets::hydrate_json(value);
    }) {
        Ok(mut prefs) => {
            super::launch_configurations::reconcile(&mut prefs);
            Ok((LoadState::Healthy(Box::new(prefs)), plaintext_in_file))
        }
        Err(payload) => {
            let info = payload.into_info(&path, recovery::backup_available(&path));
            // Content-free by construction: `reason` is an enum key and
            // `detail` carries a serde category plus a line/column. The
            // bytes themselves stay on disk — see `recovery`'s module docs.
            tracing::warn!(
                "preferences.json is corrupt ({}): {} — left on disk untouched, \
                 settings writes are refused until it is recovered",
                info.reason.as_str(),
                info.detail
            );
            Ok((LoadState::Corrupt(Box::new(info)), false))
        }
    }
}

/// The message a refused write returns.
///
/// The `PREFERENCES_CORRUPT` prefix is a stable token, deliberately at the
/// front: this string is what reaches the log and a support conversation, and
/// a user can then say "I got a PREFERENCES_CORRUPT error" instead of quoting
/// a sentence. The UI does *not* parse it — its live signal is
/// [`health`] — so nothing in the frontend depends on this wording.
pub(crate) fn corruption_error(info: &CorruptionInfo) -> String {
    format!(
        "PREFERENCES_CORRUPT: {} could not be read ({}), so it was not \
         overwritten: {}. Open Settings to restore the last-known-good backup \
         or reset the file.",
        info.path,
        info.reason.as_str(),
        info.detail
    )
}

/// The write gate: only a healthy or absent file may be written.
///
/// [`read_state`] deliberately reports corruption as a *value* so a reader
/// can serve defaults; a writer has to turn that into a refusal, and this is
/// the one place that does. Both writers ([`save`], [`try_update`]) go
/// through it, so there is no second path into [`write_to_disk`].
fn writable_state() -> Result<LoadState, String> {
    match read_state()? {
        state @ (LoadState::Healthy(_) | LoadState::Missing) => Ok(state),
        LoadState::Corrupt(info) => Err(corruption_error(&info)),
    }
}

/// Atomic, backup-refreshing write. Not a gate — call [`writable_state`]
/// first.
///
/// Private on purpose: it is the only thing that can replace the file, so
/// keeping it module-private makes [`writable_state`] structurally
/// unavoidable rather than a convention.
fn write_to_disk(prefs: &AppPreferences, previous: Option<&AppPreferences>) -> Result<(), String> {
    let _write_guard = WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = preferences_path()?;
    // Issue #830: API keys go to the credential store, not into the file or
    // its backup. `previous` lets the seam delete the entry of a key that was
    // cleared or whose account was removed.
    let on_disk = super::secrets::externalize(prefs, previous);
    let json = serde_json::to_string_pretty(&on_disk)
        .map_err(|e| format!("failed to serialize preferences: {}", e))?;
    // The last-known-good backup is refreshed from the exact bytes that just
    // landed, *after* the atomic replacement — so a backup can never hold a
    // payload that failed to deserialize, and a refused write (corrupt file)
    // can never refresh it either.
    recovery::persist_with_backup(&path, json.as_bytes())
}

/// Current health of the on-disk file, for the UI's recovery surface. Reads
/// fresh so a file the user repaired by hand is picked up without a restart.
pub fn health() -> Result<PreferencesHealth, String> {
    let path = preferences_path()?;
    let state = read_state()?;
    Ok(PreferencesHealth::from_state(&path, &state))
}

/// Explicit recovery: put the last-known-good backup back, archiving
/// whatever is on disk first.
///
/// The only path that repairs a corrupt file without discarding it — the
/// command behind it is what the Settings pane's "Restore" action calls.
pub fn restore_backup() -> Result<RecoveryOutcome, String> {
    let path = preferences_path()?;
    let mut outcome = {
        // Released before the scrub below: `write_to_disk` takes the same lock.
        let _write_guard = WRITE_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        recovery::restore_backup(&path)?
    };
    // A backup written since issue #830 holds no keys, so they come back from
    // the credential store; one from an older build still carries them and is
    // scrubbed, so the restore does not put plaintext keys back on disk.
    let plaintext_in_file = super::secrets::hydrate(&mut outcome.preferences);
    super::launch_configurations::reconcile(&mut outcome.preferences);
    scrub_plaintext_keys(plaintext_in_file);
    adopt(&outcome.preferences);
    Ok(outcome)
}

/// Explicit recovery: archive the existing file and start from defaults.
///
/// The **only** path through the app that writes defaults over a file it
/// could not read, and it is never reached implicitly — see
/// `commands::preferences::reset_app_preferences`.
pub fn reset() -> Result<RecoveryOutcome, String> {
    let path = preferences_path()?;
    let outcome = {
        let _write_guard = WRITE_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        recovery::reset_to_defaults(&path)?
    };
    // Outside the write lock: `try_update` and `load` take the cache and then
    // the write lock, so taking them in the opposite order here could deadlock.
    adopt(&outcome.preferences);
    Ok(outcome)
}

/// Publish a recovery result as the live cache and invalidate derived caches.
fn adopt(prefs: &AppPreferences) {
    set_cache(Some(prefs.clone()));
    bump_generation();
}

/// The corrupt `preferences.json`'s location, for the "open file location"
/// action. Returns the containing directory, because a file manager opening
/// a single file renders it with whichever app claims the extension — a JSON
/// editor, not Explorer.
pub fn preferences_directory() -> Result<PathBuf, String> {
    let path = preferences_path()?;
    Ok(path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(".")))
}

/// Move the plaintext API keys of a file written by an older build into the
/// credential store (issue #830), in `preferences.json` and in its
/// last-known-good backup, which holds the same secrets.
///
/// A surgical in-place edit, not a re-save: a read must not migrate the file's
/// shape or fabricate a backup. Callers have just classified the file healthy,
/// so this does not bypass the corruption gate. A failure leaves the file as it
/// was and is retried on the next start.
///
/// `primary_has_plaintext` is what the read of `preferences.json` just saw. The
/// backup is checked regardless: it can still hold keys when the primary is
/// already clean (its earlier scrub failed, or an old copy was put back), and
/// the primary's flag says nothing about it. Only the primary is authoritative:
/// a key in the backup is older than the one in the store, so it is dropped
/// rather than stored over it.
fn scrub_plaintext_keys(primary_has_plaintext: bool) {
    let Ok(path) = preferences_path() else { return };
    // The scrub reads then replaces the file, so it must exclude `write_to_disk`
    // or a settings save landing in between would be overwritten with the older
    // content. Callers must not hold `WRITE_LOCK`.
    let _write_guard = WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let targets = [
        (path.clone(), true, primary_has_plaintext),
        (recovery::backup_path(&path), false, true),
    ];
    for (file, authoritative, wanted) in targets {
        if !wanted {
            continue;
        }
        if let Err(e) = super::secrets::scrub_file(&file, authoritative) {
            tracing::warn!(
                "preferences: could not move API keys out of {}: {}",
                file.display(),
                e
            );
        }
    }
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
///
/// Issue #1523: a **corrupt** file still yields defaults here, so every
/// read-only caller (spawn routing, the circuit classifier, the usage
/// panel) keeps working instead of failing closed on a file the app can
/// still show. Those defaults are deliberately *not* published to the
/// cache: the write path re-reads the file and refuses, so no ordinary
/// settings change can turn "unreadable" into "overwritten".
pub fn load() -> Result<AppPreferences, String> {
    // Cold-cache populate, mutator, and cache publish all happen under the
    // mutex — the same contract as the pre-issue-#1386 implementation,
    // routed through the `with_cache_mut` cfg-divergent helper. The
    // closure captures the result so we don't have to thread `?` through
    // helper returns.
    let result: Result<(AppPreferences, Option<bool>), String> = with_cache_mut(|guard| {
        if let Some(cached) = guard.as_ref() {
            return Ok((cached.clone(), None));
        }
        match read_state_flagging_plaintext()? {
            (LoadState::Healthy(prefs), plaintext_in_file) => {
                *guard = Some((*prefs).clone());
                Ok((*prefs, Some(plaintext_in_file)))
            }
            (LoadState::Missing, _) => {
                let prefs = defaults();
                *guard = Some(prefs.clone());
                Ok((prefs, None))
            }
            (LoadState::Corrupt(_), _) => Ok((defaults(), None)),
        }
    });
    let (prefs, cold_healthy_read) = result?;
    // Issue #830: after the cache lock is released, so the credential-store
    // calls and file rewrite never run under it and take the write lock only on
    // their own.
    if let Some(primary_has_plaintext) = cold_healthy_read {
        scrub_plaintext_keys(primary_has_plaintext);
    }
    Ok(prefs)
}

/// Persist preferences to disk and refresh the cache.
pub fn save(mut prefs: AppPreferences) -> Result<(), String> {
    let previous = writable_state()?;
    super::launch_configurations::reconcile(&mut prefs);
    let previous = match &previous {
        LoadState::Healthy(prefs) => Some(prefs.as_ref()),
        LoadState::Missing | LoadState::Corrupt(_) => None,
    };
    write_to_disk(&prefs, previous)?;
    set_cache(Some(prefs));
    bump_generation();
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
    try_update(|prefs| {
        mutator(prefs);
        Ok(())
    })
}

pub(crate) fn try_update(
    mutator: impl FnOnce(&mut AppPreferences) -> Result<(), String>,
) -> Result<AppPreferences, String> {
    // The corruption gate runs *before* the cache lock: it is a plain
    // filesystem read, and a refused write should not have taken the lock
    // at all. Its result doubles as the cold-cache populate below, so the
    // file is read exactly once either way.
    let disk = writable_state()?;
    let result: Result<AppPreferences, String> = with_cache_mut(|guard| {
        if guard.is_none() {
            *guard = Some(match disk {
                LoadState::Healthy(prefs) => *prefs,
                // `writable_state` has already turned `Corrupt` into an
                // `Err`, so `Missing` is the only other case here.
                LoadState::Missing | LoadState::Corrupt(_) => defaults(),
            });
        }
        let previous = guard
            .as_ref()
            .expect("preferences cache was initialized")
            .clone();
        let mut candidate = previous.clone();
        mutator(&mut candidate)?;
        super::launch_configurations::reconcile(&mut candidate);
        // Publish the new cached value only after the durable atomic
        // replacement succeeds — the disk I/O is serialised by the
        // outer-mutex hold AND by `WRITE_LOCK` inside `write_to_disk`. A
        // failed write must not manufacture an in-memory verification
        // record that launch preflight could mistake for persisted proof.
        write_to_disk(&candidate, Some(&previous))?;
        *guard = Some(candidate.clone());
        Ok(candidate)
    });
    if result.is_ok() {
        bump_generation();
    }
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
