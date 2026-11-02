//! One configuration profile: its value, its synchronization and its revision.
//!
//! Issue #1482. A [`PreferencesStore`] is a clonable handle to **one immutable
//! profile directory**. It owns the in-process cache, the write coordination
//! and a monotonic revision, and it composes [`super::storage`] as the pure
//! disk adapter beneath it — the disk rules (corruption gating, backup
//! rotation, secret handling) stay in `storage`, while the *process* state
//! lives here.
//!
//! # One implementation, two entry points
//!
//! Production and tests instantiate the same type with the same `Mutex`-based
//! synchronization: a test differs only in its temporary directory. There is
//! no `cfg(test)` alternate cache in this module or in `storage`; the
//! compatibility free functions in [`super::compat`] are the only test-aware
//! layer, and they only choose *which* profile those functions address.
//!
//! # Lock order
//!
//! One order, no exceptions: cache mutex → write lock. A writer holds the cache
//! mutex across the durable replacement (so a publish cannot interleave with a
//! settings change), while the recovery actions take the write lock first and
//! never reach for the cache while holding it. No external operation — a
//! credential-store call, a backup rotation, a directory walk — runs under
//! both.

use super::model::AppPreferences;
use super::recovery::{PreferencesHealth, RecoveryOutcome};
use super::storage::{self, LoadState};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// A clonable handle to one configuration profile.
///
/// Cloning shares the exact same cache, write lock and revision. The profile
/// directory is fixed at construction and never changes, so a handle can be
/// moved into a spawned worker without the caller having to re-establish which
/// profile it reads.
///
/// The store is the only thing that publishes a new cached value: a durable
/// write lands first, then the cache and revision move together under the
/// cache mutex. A reader therefore never sees a revision that is newer than
/// the value it read (see [`PreferencesStore::snapshot`]).
#[derive(Clone, Debug)]
pub struct PreferencesStore {
    inner: Arc<StoreInner>,
}

#[derive(Debug)]
struct StoreInner {
    /// The profile's disk adapter. Its directory is immutable for the handle's
    /// lifetime, which is what makes the source identity below stable.
    disk: storage::DiskProfile,
    /// This store's identity, unique in the process. A derived cache keyed on
    /// `(source, revision)` can never treat another profile's value as its
    /// own.
    source: u64,
    /// In-process cache, refreshed on every write. Reads consult the file only
    /// if the cache is empty (first read).
    cache: Mutex<Option<AppPreferences>>,
    /// Serialises the disk replacement itself, for callers that are not
    /// already holding the cache mutex (recovery actions, the plaintext-key
    /// scrub).
    write_lock: Mutex<()>,
    /// Monotonic revision of the cached value. Only a durable publish moves
    /// it, and only while the cache mutex is held.
    revision: AtomicU64,
}

/// Identity and revision of a value read from one store.
///
/// `source` and `revision` are sampled together with `preferences`, under the
/// cache mutex, so a derived cache can use them as a key without the risk of
/// pairing a value with a revision it was not published at.
#[derive(Debug, Clone)]
pub struct StoreSnapshot {
    /// The current preferences value.
    pub preferences: AppPreferences,
    /// The identity of the store that produced this value.
    pub source: u64,
    /// The store's revision of this value. Moves forward on every durable
    /// publish; never reused by another store.
    pub revision: u64,
}

/// Process-wide identity counter for [`PreferencesStore::source`].
static NEXT_SOURCE: AtomicU64 = AtomicU64::new(1);

/// Process-wide revision sequence.
///
/// Every store seeds its revision from here and takes a fresh value for each
/// durable publish, so revisions never repeat **even across stores**. That is
/// what lets a derived cache compare revisions from two profiles and still be
/// safe: an entry published by one store can never compare equal to another
/// store's revision, so `(source, revision)` is a total identity.
static LAST_REVISION: AtomicU64 = AtomicU64::new(0);

fn next_revision() -> u64 {
    LAST_REVISION.fetch_add(1, Ordering::Relaxed) + 1
}

/// The last revision this process allocated.
///
/// Used by the compatibility layer when no store is installed, so the reported
/// generation still moves forward instead of dropping back to zero.
pub(crate) fn last_revision() -> u64 {
    LAST_REVISION.load(Ordering::Relaxed)
}

impl PreferencesStore {
    /// A store for `dir`. Does not touch the filesystem.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            inner: Arc::new(StoreInner {
                disk: storage::DiskProfile::new(dir),
                source: NEXT_SOURCE.fetch_add(1, Ordering::Relaxed),
                cache: Mutex::new(None),
                write_lock: Mutex::new(()),
                revision: AtomicU64::new(next_revision()),
            }),
        }
    }

    /// The profile directory this handle owns. Never changes.
    pub fn dir(&self) -> &Path {
        self.inner.disk.dir()
    }

    /// This handle's source identity.
    pub fn source(&self) -> u64 {
        self.inner.source
    }

    /// The store's current revision. Only durable publishes move it.
    pub fn revision(&self) -> u64 {
        self.inner.revision.load(Ordering::Relaxed)
    }

    /// `(source, revision)` for this store, sampled from one handle.
    ///
    /// A derived cache keys on the pair; taking both from the same handle is
    /// what keeps a value from one profile being filed under another's
    /// identity.
    pub fn identity(&self) -> (u64, u64) {
        (self.inner.source, self.revision())
    }

    /// `preferences.json` inside this profile.
    pub fn preferences_path(&self) -> PathBuf {
        self.inner.disk.preferences_path()
    }

    fn lock_cache(&self) -> std::sync::MutexGuard<'_, Option<AppPreferences>> {
        self.inner
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_write(&self) -> std::sync::MutexGuard<'_, ()> {
        self.inner
            .write_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Pair a cached value with the revision it is filed under. Callers must
    /// hold the cache mutex, which is the only place a revision moves.
    fn snapshot_locked(&self, preferences: AppPreferences) -> StoreSnapshot {
        StoreSnapshot {
            preferences,
            source: self.inner.source,
            revision: self.revision(),
        }
    }

    /// Read the value and revision the store currently publishes.
    ///
    /// The cold-cache read happens under the cache mutex; the value, source
    /// and revision are sampled in the same critical section so the pairing
    /// rule documented on [`StoreSnapshot`] holds structurally rather than by
    /// convention.
    pub fn snapshot(&self) -> Result<StoreSnapshot, String> {
        let (snapshot, cold_healthy_read) = {
            let mut guard = self.lock_cache();
            match guard.as_ref() {
                Some(cached) => (self.snapshot_locked(cached.clone()), None),
                None => match self.inner.disk.read_state_flagging_plaintext()? {
                    (LoadState::Healthy(prefs), plaintext_in_file) => {
                        *guard = Some((*prefs).clone());
                        (self.snapshot_locked(*prefs), Some(plaintext_in_file))
                    }
                    (LoadState::Missing, _) => {
                        let prefs = storage::defaults();
                        *guard = Some(prefs.clone());
                        (self.snapshot_locked(prefs), None)
                    }
                    (LoadState::Corrupt(_), _) => (self.snapshot_locked(storage::defaults()), None),
                },
            }
        };
        // Issue #830: after the cache lock is released, so the credential-store
        // calls and file rewrite never run under it and take the write lock only
        // on their own.
        if let Some(primary_has_plaintext) = cold_healthy_read {
            self.scrub_plaintext_keys(primary_has_plaintext);
        }
        Ok(snapshot)
    }

    /// The current value. See [`Self::snapshot`] for the read rules.
    pub fn load(&self) -> Result<AppPreferences, String> {
        Ok(self.snapshot()?.preferences)
    }

    /// Persist preferences to disk and refresh the cache.
    pub fn save(&self, mut prefs: AppPreferences) -> Result<StoreSnapshot, String> {
        let previous = self.writable_state()?;
        super::launch_configurations::reconcile(&mut prefs);
        let previous = match &previous {
            LoadState::Healthy(prefs) => Some(prefs.as_ref()),
            LoadState::Missing | LoadState::Corrupt(_) => None,
        };
        self.write_to_disk(&prefs, previous)?;
        let mut guard = self.lock_cache();
        *guard = Some(prefs.clone());
        self.publish_revision();
        Ok(self.snapshot_locked(prefs))
    }

    /// Atomically mutate the latest cached value and persist it while
    /// serialising competing read-modify-write operations on *this* store.
    ///
    /// The cache mutex is held across the cold-cache populate, the mutator,
    /// the disk write, and the publish. It serialises competing RMWs on the
    /// same profile; a second store has its own cache and write locks, so
    /// unrelated profiles never wait on each other.
    pub fn update(
        &self,
        mutator: impl FnOnce(&mut AppPreferences),
    ) -> Result<StoreSnapshot, String> {
        self.try_update(|prefs| {
            mutator(prefs);
            Ok(())
        })
    }

    /// [`Self::update`] with a fallible mutator.
    pub fn try_update(
        &self,
        mutator: impl FnOnce(&mut AppPreferences) -> Result<(), String>,
    ) -> Result<StoreSnapshot, String> {
        // The corruption gate runs *before* the cache lock: it is a plain
        // filesystem read, and a refused write should not have taken the lock
        // at all. Its result doubles as the cold-cache populate below, so the
        // file is read exactly once either way.
        let disk = self.writable_state()?;
        let mut guard = self.lock_cache();
        if guard.is_none() {
            *guard = Some(match disk {
                LoadState::Healthy(prefs) => *prefs,
                // `writable_state` has already turned `Corrupt` into an
                // `Err`, so `Missing` is the only other case here.
                LoadState::Missing | LoadState::Corrupt(_) => storage::defaults(),
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
        // replacement succeeds — the disk I/O is serialised by the outer-mutex
        // hold AND by the write lock inside `write_to_disk`. A failed write
        // must not manufacture an in-memory verification record that launch
        // preflight could mistake for persisted proof.
        self.write_to_disk(&candidate, Some(&previous))?;
        *guard = Some(candidate.clone());
        self.publish_revision();
        Ok(self.snapshot_locked(candidate))
    }

    /// Current health of this profile's on-disk file, for the UI's recovery
    /// surface. Reads fresh so a file the user repaired by hand is picked up
    /// without a restart.
    pub fn health(&self) -> Result<PreferencesHealth, String> {
        self.inner.disk.health()
    }

    /// Explicit recovery: put the last-known-good backup back, archiving
    /// whatever is on disk first.
    pub fn restore_backup(&self) -> Result<RecoveryOutcome, String> {
        let mut outcome = {
            // Released before the scrub below: `write_to_disk` takes the same
            // lock.
            let _write_guard = self.lock_write();
            self.inner.disk.restore_backup()?
        };
        // A backup written since issue #830 holds no keys, so they come back
        // from the credential store; one from an older build still carries them
        // and is scrubbed, so the restore does not put plaintext keys back on
        // disk.
        let plaintext_in_file = self.inner.disk.finish_restore(&mut outcome.preferences);
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
        let outcome = {
            let _write_guard = self.lock_write();
            self.inner.disk.reset_to_defaults()?
        };
        // Outside the write lock: `try_update` and `load` take the cache and
        // then the write lock, so taking them in the opposite order here could
        // deadlock.
        self.adopt(&outcome.preferences);
        Ok(outcome)
    }

    /// Publish a recovery result as the live cache and advance the revision.
    fn adopt(&self, prefs: &AppPreferences) {
        let mut guard = self.lock_cache();
        *guard = Some(prefs.clone());
        self.publish_revision();
    }

    /// Record a durable publish. Callers hold the cache mutex, so a concurrent
    /// [`Self::snapshot`] cannot pair this revision with the previous value.
    fn publish_revision(&self) {
        self.inner
            .revision
            .store(next_revision(), Ordering::Relaxed);
    }

    /// Test-only raw read of this profile's on-disk state. Never mutates the
    /// cache — the recovery suites assert on [`LoadState`] itself.
    #[cfg(test)]
    pub(crate) fn read_state(&self) -> Result<LoadState, String> {
        self.inner.disk.read_state()
    }

    /// Move this profile's plaintext API keys into the credential store (issue
    /// #830), in `preferences.json` and in its last-known-good backup.
    fn scrub_plaintext_keys(&self, primary_has_plaintext: bool) {
        // The scrub reads then replaces the file, so it must exclude
        // `write_to_disk` or a settings save landing in between would be
        // overwritten with the older content. Callers must not hold the write
        // lock.
        let _write_guard = self.lock_write();
        self.inner.disk.scrub_plaintext_keys(primary_has_plaintext);
    }

    /// The write gate for this profile: only a healthy or absent file may be
    /// written. See [`storage::DiskProfile::writable_state`].
    fn writable_state(&self) -> Result<LoadState, String> {
        self.inner.disk.writable_state()
    }

    /// Atomic, backup-refreshing write under this store's write lock.
    fn write_to_disk(
        &self,
        prefs: &AppPreferences,
        previous: Option<&AppPreferences>,
    ) -> Result<(), String> {
        let _write_guard = self.lock_write();
        self.inner.disk.write_to_disk(prefs, previous)
    }
}
