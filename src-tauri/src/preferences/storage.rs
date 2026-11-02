//! The disk adapter: reading, classifying and atomically replacing one
//! profile's `preferences.json`.
//!
//! This module is the **boundary** between the in-memory value and durable
//! storage, and it owns nothing but bytes: no cache, no lock, no revision.
//! [`super::store::PreferencesStore`] composes a [`DiskProfile`] with those
//! (issue #1482), so the disk rules below have one home and the process state
//! has another.
//!
//! Two rules are worth stating up front, because they are the reason this is
//! not a thin `serde_json` wrapper:
//!
//! * **A read never persists.** A corrupt or unreadable file is reported, not
//!   repaired, and never replaced by defaults.
//! * **Only a healthy or absent file may be written** ([`LoadState`], issue
//!   #1523). The gate lives on the writer path, so no ordinary settings change
//!   can turn "unreadable" into "overwritten".
//!
//! The bytes on disk are owned by [`super::recovery`]: it classifies a
//! payload, keeps the last-known-good backup, and performs the explicit
//! recovery actions. `recovery::classify` is pure, so the classification
//! rules are testable without a filesystem, while the stateful decision
//! ("may this write land?") has exactly one home — [`DiskProfile::writable_state`].

use super::model::AppPreferences;
use super::recovery::{self, CorruptionInfo, PreferencesHealth, RecoveryOutcome};
use std::path::{Path, PathBuf};

/// The on-disk half of one configuration profile.
///
/// Every method is a filesystem operation on `preferences.json` and its
/// last-known-good backup inside an immutable directory. The handle is cheap to
/// clone; it carries no state at all.
#[derive(Debug, Clone)]
pub(crate) struct DiskProfile {
    dir: PathBuf,
}

impl DiskProfile {
    pub(crate) fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The profile directory. Immutable for the handle's lifetime.
    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    /// `preferences.json` inside this profile.
    pub(crate) fn preferences_path(&self) -> PathBuf {
        self.dir.join("preferences.json")
    }

    /// The credential-store profile for this directory (its leaf name).
    fn profile(&self) -> String {
        super::secrets::profile_for(&self.dir)
    }

    /// Current health of this profile's on-disk file, for the UI's recovery
    /// surface. Reads fresh so a file the user repaired by hand is picked up
    /// without a restart.
    pub(crate) fn health(&self) -> Result<PreferencesHealth, String> {
        let path = self.preferences_path();
        let state = self.read_state()?;
        Ok(PreferencesHealth::from_state(&path, &state))
    }

    /// Explicit recovery: put the last-known-good backup back, archiving
    /// whatever is on disk first. Lock-free; the caller holds the write lock.
    pub(crate) fn restore_backup(&self) -> Result<RecoveryOutcome, String> {
        recovery::restore_backup(&self.preferences_path())
    }

    /// Explicit recovery: archive the existing file and start from defaults.
    /// Lock-free; the caller holds the write lock.
    pub(crate) fn reset_to_defaults(&self) -> Result<RecoveryOutcome, String> {
        recovery::reset_to_defaults(&self.preferences_path())
    }

    /// Finish a restore: put the stored keys back and re-derive launch
    /// availability. Returns whether the restored bytes still carried a
    /// plaintext key — the caller's cue to [`Self::scrub_plaintext_keys`].
    pub(crate) fn finish_restore(&self, prefs: &mut AppPreferences) -> bool {
        let plaintext_in_file = super::secrets::hydrate(prefs, &self.profile());
        super::launch_configurations::reconcile(prefs);
        plaintext_in_file
    }

    /// Move this profile's plaintext API keys into the credential store (issue
    /// #830), in `preferences.json` and in its last-known-good backup.
    ///
    /// Lock-free: the caller serialises it against writers by holding the
    /// store's write lock.
    pub(crate) fn scrub_plaintext_keys(&self, primary_has_plaintext: bool) {
        let path = self.preferences_path();
        let profile = self.profile();
        let targets = [
            (path.clone(), true, primary_has_plaintext),
            (recovery::backup_path(&path), false, true),
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

    /// Read and classify the on-disk file. Never writes.
    pub(crate) fn read_state(&self) -> Result<LoadState, String> {
        self.read_state_flagging_plaintext().map(|(state, _)| state)
    }

    /// [`Self::read_state`] plus whether the file still held a plaintext API
    /// key (issue #830) — the cue for [`Self::scrub_plaintext_keys`].
    pub(crate) fn read_state_flagging_plaintext(&self) -> Result<(LoadState, bool), String> {
        let path = self.preferences_path();
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
        let profile = self.profile();
        match recovery::classify_with(&raw, |value| {
            plaintext_in_file = super::secrets::hydrate_json(value, &profile);
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

    /// The write gate: only a healthy or absent file may be written.
    ///
    /// [`Self::read_state`] deliberately reports corruption as a *value* so a
    /// reader can serve defaults; a writer has to turn that into a refusal, and
    /// this is the one place that does. Every writer goes through it, so there
    /// is no second path into [`Self::write_to_disk`].
    pub(crate) fn writable_state(&self) -> Result<LoadState, String> {
        match self.read_state()? {
            state @ (LoadState::Healthy(_) | LoadState::Missing) => Ok(state),
            LoadState::Corrupt(info) => Err(corruption_error(&info)),
        }
    }

    /// Atomic, backup-refreshing write. Not a gate — call
    /// [`Self::writable_state`] first, and hold the profile's write lock.
    ///
    /// Private on purpose: it is the only thing that can replace the file, so
    /// keeping it off the public surface makes the gate structurally
    /// unavoidable rather than a convention.
    pub(crate) fn write_to_disk(
        &self,
        prefs: &AppPreferences,
        previous: Option<&AppPreferences>,
    ) -> Result<(), String> {
        let path = self.preferences_path();
        // Issue #830: API keys go to the credential store, not into the file or
        // its backup. `previous` lets the seam delete the entry of a key that was
        // cleared or whose account was removed.
        let on_disk = super::secrets::externalize(prefs, previous, &self.profile());
        let json = serde_json::to_string_pretty(&on_disk)
            .map_err(|e| format!("failed to serialize preferences: {}", e))?;
        // The last-known-good backup is refreshed from the exact bytes that just
        // landed, *after* the atomic replacement — so a backup can never hold a
        // payload that failed to deserialize, and a refused write (corrupt file)
        // can never refresh it either.
        recovery::persist_with_backup(&path, json.as_bytes())
    }
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
pub(crate) fn defaults() -> AppPreferences {
    let mut prefs = AppPreferences::default();
    super::launch_configurations::reconcile(&mut prefs);
    prefs
}

/// The message a refused write returns.
///
/// The `PREFERENCES_CORRUPT` prefix is a stable token, deliberately at the
/// front: this string is what reaches the log and a support conversation, and
/// a user can then say "I got a PREFERENCES_CORRUPT error" instead of quoting
/// a sentence. The UI does *not* parse it — its live signal is `health()` — so
/// nothing in the frontend depends on this wording.
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

// ---------------------------------------------------------------------------
// Migration alias (issue #1482).
//
// The compatibility free functions live in [`super::compat`]; they are
// re-exported here because unmigrated call sites address them as
// `preferences::storage::load(..)`. Delete these re-exports as call sites move
// to explicit [`super::store::PreferencesStore`] handles.
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) use super::compat::{
    default_provider, generation, health, init_for_tests, preferences_path, read_state, reset,
    reset_for_tests, restore_backup, reviewer_provider, update,
};
pub(crate) use super::compat::{load, save, try_update};
