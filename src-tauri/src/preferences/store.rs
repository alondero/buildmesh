//! One profile's preferences: the persisted value, its synchronization, its
//! revision, and the identity a derived cache keys on.
//!
//! Issue #1482. The preferences state used to live in four process-wide
//! statics — `APP_DATA_DIR`, `CACHE`, `WRITE_LOCK` and `GENERATION` — whose
//! test counterparts were thread-local `RefCell`s. That gave production and
//! tests **two different synchronization implementations of the same
//! behaviour**: the code under test called a primitive production never runs,
//! so no test could establish what production does, and a test that spawned a
//! worker thread silently read an uninitialized source. The derived caches
//! outside this module (`agent::launch_routing`, `agent::provider_menu`)
//! repeated the same fork.
//!
//! [`PreferencesStore`] is the seam that removes it. A store is a clonable
//! handle to **one immutable profile directory**:
//!
//! * the cached value and the revision it was published at live together
//!   under one mutex, so a reader can never pair a value with another
//!   revision;
//! * the durable-write coordination lives in the store, so a clone shares the
//!   same cache and the same locks;
//! * the [`SourceIdentity`] names the profile the store owns, so a derived
//!   cache can refuse an entry computed against another profile or against an
//!   older revision.
//!
//! Production and tests use **the same implementation**: the cache and the
//! write lock are `std::sync::Mutex` in every build. A test differs only in
//! the directory it constructs the store over and in the handle it passes
//! around — never in how the store synchronizes.
//!
//! See the [module-level docs](super) for what concerns each submodule owns.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use super::model::AppPreferences;
use super::recovery::{PreferencesHealth, RecoveryOutcome};
use super::storage::{self, LoadState};

/// Hands out a unique id per store instance.
///
/// A store is identified by the *instance*, not by its directory: two stores
/// over the same directory are still two sources, each with its own cache,
/// revision and locks. Their revisions are therefore not comparable, and
/// neither may serve the other's derived-cache entries.
static STORE_IDS: AtomicU64 = AtomicU64::new(1);

/// Identity of the preference source a [`PreferencesStore`] owns.
///
/// Derived caches key on this alongside the store's revision, so an entry
/// computed against another profile — or against an older revision of this
/// profile — can never become a hit.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceIdentity {
    id: u64,
    profile_dir: PathBuf,
}

impl Default for SourceIdentity {
    /// The default identity is the *unbound* one — no preference source.
    ///
    /// A default-constructed snapshot therefore cannot be attributed to a
    /// profile, and a derived cache keyed on it must not serve an entry it
    /// did not compute for itself.
    fn default() -> Self {
        SourceIdentity::unbound()
    }
}

impl SourceIdentity {
    /// The profile directory this source reads and writes.
    pub fn profile_dir(&self) -> &Path {
        &self.profile_dir
    }

    /// The identity of "no preference source at all" — the preferences
    /// module was never initialised.
    ///
    /// A snapshot carrying this identity cannot be attributed to a profile,
    /// so a derived cache keyed on it must never serve an entry it did not
    /// compute for itself. The id is `0`, which no real store carries
    /// ([`STORE_IDS`] starts at `1`).
    pub fn unbound() -> Self {
        SourceIdentity {
            id: 0,
            profile_dir: PathBuf::new(),
        }
    }
}

/// The cached value and the revision it was published at.
///
/// Both live under one mutex so a reader can never observe a value paired
/// with another revision (issue #1482, invariant 4).
#[derive(Default)]
struct CacheState {
    cached: Option<AppPreferences>,
    revision: u64,
}

struct StoreInner {
    /// The one directory this store owns. It never changes after
    /// construction — there is no path setter, so a clone always addresses
    /// the same profile. The same path is carried by
    /// [`SourceIdentity::profile_dir`]; the store reads it from there, so
    /// there is one source of truth for it.
    source: SourceIdentity,
    /// The cached value and its revision.
    state: Mutex<CacheState>,
    /// Durable-write coordination. Held *after* `state`, never before
    /// (see the lock-order note on [`PreferencesStore::try_update`]).
    write_lock: Mutex<()>,
}

/// A clonable handle to one profile's persisted preferences.
///
/// Cloning shares the **exact same** cache, revision and locks — the handle
/// is a pointer to the store, not a copy of it. That is what makes a handle
/// safe to hand to a spawned worker: the worker reads the same profile the
/// parent wrote, instead of an uninitialized test-local source.
///
/// The `preferences` module is private to the crate, so this example is not
/// compiled as a doctest — an external crate cannot name the type.
///
/// ```ignore
/// let store = preferences::store::PreferencesStore::new(profile_dir);
/// let worker = store.clone();
/// std::thread::spawn(move || worker.load()).join().unwrap();
/// ```
#[derive(Clone)]
pub struct PreferencesStore {
    inner: Arc<StoreInner>,
}

impl std::fmt::Debug for PreferencesStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreferencesStore")
            .field("source", &self.inner.source)
            .field("revision", &self.revision())
            .finish()
    }
}

impl PreferencesStore {
    /// Construct a store over one profile directory.
    ///
    /// The directory is the store's identity for its whole life: it never
    /// changes, so every clone addresses the same profile. The directory is
    /// created on the first durable write, not here — a store over a
    /// not-yet-existing profile is a fresh install, not an error.
    pub fn new(profile_dir: PathBuf) -> Self {
        let id = STORE_IDS.fetch_add(1, Ordering::Relaxed);
        let source = SourceIdentity { id, profile_dir };
        PreferencesStore {
            inner: Arc::new(StoreInner {
                source,
                state: Mutex::new(CacheState::default()),
                write_lock: Mutex::new(()),
            }),
        }
    }

    /// The identity of the preference source this store owns.
    pub fn source(&self) -> &SourceIdentity {
        &self.inner.source
    }

    /// The profile directory this store owns.
    pub fn profile_dir(&self) -> &Path {
        self.inner.source.profile_dir()
    }

    /// The revision the cached value was published at.
    ///
    /// A revision advances only after a durable write has landed, so a
    /// revision identifies a *persisted* value — never one that a failed
    /// write manufactured.
    pub fn revision(&self) -> u64 {
        self.lock_state().revision
    }

    /// The path of the preferences file this store owns.
    pub fn preferences_path(&self) -> PathBuf {
        self.profile_dir().join("preferences.json")
    }

    /// The directory holding the preferences file, for the "open file
    /// location" action. The *containing* directory, because a file manager
    /// opening a single file renders it with whichever app claims the
    /// extension — a JSON editor, not Explorer.
    pub fn preferences_directory(&self) -> PathBuf {
        self.profile_dir().to_path_buf()
    }

    fn lock_state(&self) -> MutexGuard<'_, CacheState> {
        // A panic in a previous holder would have released the guard on
        // unwind, leaving the inner value consistent (None-or-fully-
        // populated). `.unwrap()` on `PoisonError` would brick every
        // subsequent read and freeze the whole preferences surface;
        // `into_inner()` lets the next caller decide whether to refresh from
        // disk (issue #1224).
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_write(&self) -> MutexGuard<'_, ()> {
        self.inner
            .write_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Provider-account ids whose API key is persisted in `preferences.json`
    /// instead of the OS credential store (issue #2154).
    ///
    /// The read path deliberately cannot answer this — once the value has
    /// been hydrated a key looks the same whichever side it came from — so
    /// this reads the files themselves. Empty means every key is in the
    /// credential store, which is the state that must show no notice.
    pub fn provider_accounts_with_keys_in_preferences(&self) -> Vec<String> {
        super::secrets::account_ids_with_keys_on_disk(&self.preferences_path())
    }

    /// The credential namespace of this profile.
    /// The credential store is shared by every profile on the machine, so the
    /// entry names carry the profile's leaf — the bundle identifier. The
    /// namespace follows *this* store, so a store over another directory
    /// never reads or writes another profile's vault.
    pub fn credential_profile(&self) -> String {
        super::secrets::profile_for(self.profile_dir())
    }

    /// Load preferences, populating the in-process cache on first call.
    ///
    /// The cold-cache populate, the cache publish and the revision sample all
    /// happen under the store's own mutex, so the returned revision always
    /// belongs to the returned value.
    pub fn load(&self) -> Result<AppPreferences, String> {
        self.load_with_revision().map(|(prefs, _)| prefs)
    }

    /// [`Self::load`], plus the revision the value was published at.
    ///
    /// The two are sampled **together** under the store's lock, so they
    /// always describe the same publication.
    pub fn load_with_revision(&self) -> Result<(AppPreferences, u64), String> {
        #[cfg(test)]
        // allow-test-in-production: the counter's writer, called from the one read path it counts.
        count_load();
        let profile = self.credential_profile();
        let result: Result<(AppPreferences, Option<bool>, u64), String> = {
            let mut guard = self.lock_state();
            if let Some(cached) = guard.cached.clone() {
                Ok((cached, None, guard.revision))
            } else {
                let (state, plaintext_in_file) =
                    storage::read_state_flagging_plaintext(&self.preferences_path(), &profile)?;
                match state {
                    LoadState::Healthy(prefs) => {
                        guard.cached = Some((*prefs).clone());
                        Ok(((*prefs).clone(), Some(plaintext_in_file), guard.revision))
                    }
                    // A fresh install may seed the writable cache with
                    // defaults (issue #1523).
                    LoadState::Missing => {
                        let prefs = storage::defaults();
                        guard.cached = Some(prefs.clone());
                        Ok((prefs, None, guard.revision))
                    }
                    // A corrupt file still yields defaults here, so every
                    // read-only caller keeps working. Those defaults are
                    // deliberately *not* published to the cache: the write
                    // path re-reads the file and refuses, so no ordinary
                    // settings change can turn "unreadable" into
                    // "overwritten".
                    LoadState::Corrupt(_) => Ok((storage::defaults(), None, guard.revision)),
                }
            }
        };
        let (prefs, cold_healthy_read, revision) = result?;
        // Issue #830: after the cache lock is released, so the credential-store
        // calls and file rewrite never run under it and take the write lock
        // only on their own.
        if let Some(primary_has_plaintext) = cold_healthy_read {
            self.scrub_plaintext_keys(primary_has_plaintext);
        }
        Ok((prefs, revision))
    }

    /// Persist preferences to disk and publish them as the live value.
    ///
    /// The durable replacement happens **before** the value and the revision
    /// are published, so a failed write cannot manufacture a value that
    /// looks persisted.
    ///
    /// **Lock order:** as in [`Self::try_update`], the state lock is taken
    /// first and the write lock second, and both are held across the durable
    /// write and the publish. Holding the state lock across the write is
    /// what keeps the cache and the file from disagreeing: two concurrent
    /// saves cannot both land — the second replaces the first's value on disk
    /// and publishes that same value, so the cache and the file always agree.
    pub fn save(&self, mut prefs: AppPreferences) -> Result<(), String> {
        // The corruption gate runs *before* the state lock: it is a plain
        // filesystem read, and a refused write should not have taken the
        // lock at all.
        let profile = self.credential_profile();
        let previous = storage::writable_state(&self.preferences_path(), &profile)?;
        super::launch_configurations::reconcile(&mut prefs);
        let previous = match &previous {
            LoadState::Healthy(prefs) => Some(prefs.as_ref()),
            LoadState::Missing | LoadState::Corrupt(_) => None,
        };
        let mut guard = self.lock_state();
        self.write_to_disk(&prefs, previous)?;
        guard.cached = Some(prefs);
        guard.revision += 1;
        Ok(())
    }

    /// Atomically mutate the latest cached preference value and persist it
    /// while serialising competing read-modify-write operations.
    pub fn update(
        &self,
        mutator: impl FnOnce(&mut AppPreferences),
    ) -> Result<AppPreferences, String> {
        self.try_update(|prefs| {
            mutator(prefs);
            Ok(())
        })
    }

    /// [`Self::update`], with a mutator that may refuse the change.
    ///
    /// **Lock order:** the state lock is taken first and the write lock
    /// second, and both are held across the mutator, the durable write and
    /// the publish. Holding the state lock across the disk write is what
    /// makes competing read-modify-write operations serialise: releasing it
    /// between the mutator and the write would let two concurrent updaters
    /// both win the in-memory race against the on-disk one.
    ///
    /// The reverse order is never taken. `restore_backup` and `reset` need
    /// the write lock on its own, so they release it before publishing.
    pub fn try_update(
        &self,
        mutator: impl FnOnce(&mut AppPreferences) -> Result<(), String>,
    ) -> Result<AppPreferences, String> {
        // The corruption gate runs *before* the state lock: it is a plain
        // filesystem read, and a refused write should not have taken the
        // lock at all. Its result doubles as the cold-cache populate below,
        // so the file is read exactly once either way.
        let profile = self.credential_profile();
        let disk = storage::writable_state(&self.preferences_path(), &profile)?;
        let mut guard = self.lock_state();
        if guard.cached.is_none() {
            guard.cached = Some(match disk {
                LoadState::Healthy(prefs) => *prefs,
                // `writable_state` has already turned `Corrupt` into an
                // `Err`, so `Missing` is the only other case here.
                LoadState::Missing | LoadState::Corrupt(_) => storage::defaults(),
            });
        }
        let previous = guard
            .cached
            .clone()
            .expect("preferences cache was initialized");
        let mut candidate = previous.clone();
        mutator(&mut candidate)?;
        super::launch_configurations::reconcile(&mut candidate);
        // Publish the new cached value only after the durable atomic
        // replacement succeeds. A failed write must not manufacture an
        // in-memory verification record that launch preflight could mistake
        // for persisted proof.
        self.write_to_disk(&candidate, Some(&previous))?;
        guard.cached = Some(candidate.clone());
        guard.revision += 1;
        Ok(candidate)
    }

    /// Current health of the on-disk file, for the UI's recovery surface.
    /// Reads fresh so a file the user repaired by hand is picked up without
    /// a restart.
    pub fn health(&self) -> Result<PreferencesHealth, String> {
        let path = self.preferences_path();
        let profile = self.credential_profile();
        let state = storage::read_state(&path, &profile)?;
        Ok(PreferencesHealth::from_state(&path, &state))
    }

    /// Explicit recovery: put the last-known-good backup back, archiving
    /// whatever is on disk first.
    ///
    /// The only path that repairs a corrupt file without discarding it — the
    /// command behind it is what the Settings pane's "Restore" action calls.
    pub fn restore_backup(&self) -> Result<RecoveryOutcome, String> {
        let path = self.preferences_path();
        let profile = self.credential_profile();
        let mut outcome = {
            // Released before the scrub and the publish below: both take the
            // write lock and the state lock, and the state lock is always
            // taken first (see [`Self::try_update`]).
            let _write_guard = self.lock_write();
            super::recovery::restore_backup(&path)?
        };
        // A backup written since issue #830 holds no keys, so they come back
        // from the credential store; one from an older build still carries
        // them and is scrubbed, so the restore does not put plaintext keys
        // back on disk.
        let plaintext_in_file = super::secrets::hydrate(&mut outcome.preferences, &profile);
        super::launch_configurations::reconcile(&mut outcome.preferences);
        self.scrub_plaintext_keys(plaintext_in_file);
        self.adopt(&outcome.preferences);
        Ok(outcome)
    }

    /// Explicit recovery: archive the existing file and start from defaults.
    ///
    /// The **only** path through the app that writes defaults over a file it
    /// could not read, and it is never reached implicitly — see
    /// `commands::preferences::reset_app_preferences`.
    pub fn reset(&self) -> Result<RecoveryOutcome, String> {
        let path = self.preferences_path();
        let outcome = {
            let _write_guard = self.lock_write();
            super::recovery::reset_to_defaults(&path)?
        };
        // Outside the write lock: `try_update` and `load` take the state lock
        // and then the write lock, so taking them in the opposite order here
        // could deadlock.
        self.adopt(&outcome.preferences);
        Ok(outcome)
    }

    /// Publish a recovery result as the live value and advance the revision.
    fn adopt(&self, prefs: &AppPreferences) {
        let mut guard = self.lock_state();
        guard.cached = Some(prefs.clone());
        guard.revision += 1;
    }

    /// Atomic, backup-refreshing write. Not a gate — the caller checks
    /// [`storage::writable_state`] first.
    ///
    /// Takes the write lock itself; the caller must not hold it.
    fn write_to_disk(
        &self,
        prefs: &AppPreferences,
        previous: Option<&AppPreferences>,
    ) -> Result<(), String> {
        let _write_guard = self.lock_write();
        let profile = self.credential_profile();
        // Issue #830: API keys go to the credential store, not into the file
        // or its backup. `previous` lets the seam delete the entry of a key
        // that was cleared or whose account was removed.
        let on_disk = super::secrets::externalize(prefs, previous, &profile);
        let json = serde_json::to_string_pretty(&on_disk)
            .map_err(|e| format!("failed to serialize preferences: {}", e))?;
        // The last-known-good backup is refreshed from the exact bytes that
        // just landed, *after* the atomic replacement — so a backup can never
        // hold a payload that failed to deserialize, and a refused write
        // (corrupt file) can never refresh it either.
        super::recovery::persist_with_backup(&self.preferences_path(), json.as_bytes())
    }

    /// Move the plaintext API keys of a file written by an older build into
    /// the credential store (issue #830), in `preferences.json` and in its
    /// last-known-good backup, which holds the same secrets.
    ///
    /// A surgical in-place edit, not a re-save: a read must not migrate the
    /// file's shape or fabricate a backup. Callers have just classified the
    /// file healthy, so this does not bypass the corruption gate. A failure
    /// leaves the file as it was and is retried on the next start.
    ///
    /// `primary_has_plaintext` is what the read of `preferences.json` just
    /// saw. The backup is checked regardless: it can still hold keys when the
    /// primary is already clean (its earlier scrub failed, or an old copy was
    /// put back), and the primary's flag says nothing about it. Only the
    /// primary is authoritative: a key in the backup is older than the one in
    /// the store, so it is dropped rather than stored over it.
    fn scrub_plaintext_keys(&self, primary_has_plaintext: bool) {
        let path = self.preferences_path();
        let profile = self.credential_profile();
        // The scrub reads then replaces the file, so it must exclude
        // `write_to_disk` or a settings save landing in between would be
        // overwritten with the older content. Callers must not hold the
        // state lock.
        let _write_guard = self.lock_write();
        let targets = [
            (path.clone(), true, primary_has_plaintext),
            (super::recovery::backup_path(&path), false, true),
        ];
        for (file, authoritative, wanted) in targets {
            if !wanted {
                continue;
            }
            if let Err(e) = super::secrets::scrub_file(&file, authoritative, &profile) {
                tracing::warn!(
                    "preferences: could not move API keys out of {}: {}",
                    file.display(),
                    e
                );
            }
        }
    }

    /// Read the preferences once and resolve the effective view, stamped with
    /// this store's identity and revision.
    pub fn snapshot(&self) -> Result<super::resolver::PreferencesSnapshot, String> {
        super::load_snapshot_from(self)
    }

    /// The app-wide default provider id, if any.
    ///
    /// Empty strings are treated as `None` to match how the per-mesh column
    /// is normalized elsewhere (see `commands::mesh::get_default_provider`).
    ///
    /// A load failure (e.g. an unreadable file) is logged once and treated
    /// as "no override". We don't propagate the error because the precedence
    /// chain has a hardcoded fallback — but without the warn! a
    /// misconfigured environment would silently ignore the user's setting
    /// with no trace.
    pub fn default_provider(&self) -> Option<String> {
        match self.load() {
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

    /// The optional app-wide reviewer Spawn Option. A blank value means
    /// reviews inherit the source/parent provider, while a configured value
    /// lets an adversarial review use an independent harness.
    pub fn reviewer_provider(&self) -> Option<String> {
        match self.load() {
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

    /// The user-configured backend the session-naming helper uses to
    /// summarise PTY output into a slug (issue #824). `None` means
    /// "auto-naming is off" — the session_naming module short-circuits and
    /// nodes retain their random `adjective-adjective-noun` slug. An empty
    /// string is normalised to `None` here so a save with `""` from the
    /// frontend acts the same as a clear.
    ///
    /// Distinct from [`Self::default_provider`]: the naming helper runs
    /// frequently on content that is well below the front-line model's
    /// intelligence threshold, so the user explicitly opts in via Settings
    /// rather than inheriting whatever provider a spawned node happens to be
    /// on (which can be an expensive tier like Opus with xhigh effort).
    ///
    /// The naming helper treats this as a *spawn-option id* (e.g. `"minimax"`,
    /// `"claude:minimax"`, `"claude:openrouter"`) and resolves it through
    /// [`crate::preferences::resolve_provider_env`] the same way node spawns
    /// do — so a user who already configured a Provider Account can reuse it
    /// here for free. Built-in Anthropic (`"anthropic"`) is special-cased
    /// inside `session_naming` to pin a cheap haiku tier; the historical
    /// `minimax_backend_env()` side-channel is no longer the implicit
    /// default.
    pub fn naming_provider(&self) -> Option<String> {
        match self.load() {
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
    /// A load failure logs and falls back to `None` like
    /// [`Self::default_provider`].
    pub fn worktree_directory(&self) -> Option<String> {
        match self.load() {
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

    /// The global autopilot pool size — the app-wide cap on concurrently
    /// active autopilot nodes across every mesh (see
    /// [`AppPreferences::circuit_agent_pool_size`]). `None` means "no global
    /// cap" — the per-mesh `autopilot_concurrency_limit` values are the only
    /// gate, which was the behaviour before this setting existed. A load
    /// failure is logged and treated as "no cap" for the same reason as
    /// [`Self::default_provider`]: the poller must keep working even when
    /// preferences are unreadable.
    pub fn circuit_agent_pool_size(&self) -> Option<u32> {
        match self.load() {
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
    /// `None` like [`Self::default_provider`].
    pub fn issue_spawn_prompt(&self) -> Option<String> {
        match self.load() {
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
    /// as [`Self::issue_spawn_prompt`].
    pub fn pr_spawn_prompt(&self) -> Option<String> {
        match self.load() {
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
}

// ---------------------------------------------------------------------------
// The ambient store
// ---------------------------------------------------------------------------
//
// The free functions below are the compatibility surface the rest of the
// codebase uses. Each delegates to the *ambient* store: the process-wide one
// installed by [`init`] in production. It is the *only* ambient store: there
// is no test-only override, because issue #1482 requires a test to construct
// and pass its own [`PreferencesStore`] handle rather than swap a
// process-wide current store or use a thread-local override.
//
// Production installs exactly one store, at startup. A test constructs its own
// store over its own temporary directory — the *implementation* is identical
// in both cases (same mutexes, same locks, same revision rules); only the
// instance differs.

/// The process-wide store, installed during startup.
static DEFAULT_STORE: Mutex<Option<PreferencesStore>> = Mutex::new(None);

/// Install the process-wide store for `profile_dir`.
///
/// Called once from `lib.rs::setup`, before anything reads preferences. The
/// store's directory never changes afterwards — there is no path setter.
pub fn init(profile_dir: PathBuf) {
    let store = PreferencesStore::new(profile_dir);
    *DEFAULT_STORE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(store);
}

/// The process-wide store, if one has been installed.
pub fn default_store() -> Option<PreferencesStore> {
    DEFAULT_STORE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// The store the free functions resolve.
///
/// There is no test-only override: a test that owns its state constructs a
/// [`PreferencesStore`] and passes the handle to the code under test.
pub(crate) fn ambient_store() -> Result<PreferencesStore, String> {
    default_store().ok_or_else(|| "preferences module not initialized".to_string())
}

/// The app-data directory the ambient store was wired to, for sibling config
/// files that live next to `preferences.json` (e.g. Autopilot's `finish.md`,
/// issue #484). `None` before the store is installed.
pub fn app_data_dir() -> Option<PathBuf> {
    default_store().map(|store| store.profile_dir().to_path_buf())
}

/// The credential namespace of the ambient store's profile.
///
/// The credential store is shared by every profile on the machine, so the
/// entry names carry the profile's leaf — the bundle identifier.
pub fn credential_profile() -> String {
    ambient_store()
        .map(|store| store.credential_profile())
        .unwrap_or_else(|_| "default".to_string())
}

// ----- Free-function façade ----------------------------------------------
//
// Each of these delegates to the ambient store. They are the migration
// surface: production callers that have not yet been given an explicit
// handle come through here, and the storage semantics they observe are the
// store's semantics, not a second implementation of them.

/// Load preferences, populating the in-process cache on first call.
pub fn load() -> Result<AppPreferences, String> {
    ambient_store()?.load()
}

/// Persist preferences to disk and refresh the cache.
pub fn save(prefs: AppPreferences) -> Result<(), String> {
    ambient_store()?.save(prefs)
}

/// Atomically mutate the latest cached preference value and persist it.
pub fn update(mutator: impl FnOnce(&mut AppPreferences)) -> Result<AppPreferences, String> {
    ambient_store()?.update(mutator)
}

/// Current health of the on-disk file, for the UI's recovery surface.
pub fn health() -> Result<PreferencesHealth, String> {
    ambient_store()?.health()
}

/// Explicit recovery: put the last-known-good backup back.
pub fn restore_backup() -> Result<RecoveryOutcome, String> {
    ambient_store()?.restore_backup()
}

/// Explicit recovery: archive the existing file and start from defaults.
pub fn reset() -> Result<RecoveryOutcome, String> {
    ambient_store()?.reset()
}

/// The directory holding the corrupt `preferences.json`, for the "open file
/// location" action.
pub fn preferences_directory() -> Result<PathBuf, String> {
    Ok(ambient_store()?.preferences_directory())
}

/// Convenience: returns the app-wide default provider id, if any.
/// Empty strings are treated as `None` to match how the per-mesh column
/// is normalized elsewhere (see `commands::mesh::get_default_provider`).
///
/// A load failure (e.g. preferences module not initialised, or unreadable
/// file) is logged once and treated as "no override". We don't propagate the
/// error because the precedence chain has a hardcoded fallback — but
/// without the warn! a misconfigured environment would silently ignore the
/// user's setting with no trace.
pub fn default_provider() -> Option<String> {
    match ambient_store() {
        Ok(store) => store.default_provider(),
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
    match ambient_store() {
        Ok(store) => store.reviewer_provider(),
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
    match ambient_store() {
        Ok(store) => store.naming_provider(),
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
    match ambient_store() {
        Ok(store) => store.worktree_directory(),
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
    match ambient_store() {
        Ok(store) => store.circuit_agent_pool_size(),
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
    match ambient_store() {
        Ok(store) => store.issue_spawn_prompt(),
        Err(e) => {
            tracing::warn!(
                "preferences::issue_spawn_prompt load failed, falling back to default: {}",
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
/// before #575 lands keeps the legacy bare form in their preferences.json —
/// and the bare form routes through `resolve_provider_env` to the keyed
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
/// alone. On a no-op (the common case after the first launch) this is
/// a single cached read plus an equality check.
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

// ---------------------------------------------------------------------------
// Test support
// ---------------------------------------------------------------------------

// There is no test-only ambient store: a test constructs its own
// [`PreferencesStore`] over a temporary directory and passes the handle to the
// code under test (issue #1482). The only test-only state left here is the
// load counter below, which is instrumentation and not storage semantics.

// Counts `load` calls on this thread so a test can assert that a derivation
// reads preferences once (issue #2532).
//
// Per-thread: a parallel test on another worker thread must not change this
// thread's count. The counter is instrumentation only — the storage semantics
// it sits next to are the production ones.
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

/// The number of [`load`] calls made on this thread.
#[cfg(test)]
// allow-test-in-production: the counter's reader, called from a sibling suite's assertion.
pub(crate) fn load_count() -> usize {
    LOAD_COUNT.with(|count| count.get())
}
