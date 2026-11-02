//! Disk persistence: classification, atomic write coordination, recovery gates.
//!
//! This module is the **disk adapter**. It owns the rules that decide what a
//! caller is *allowed* to do with a file that is not readable
//! ([`LoadState`], issue #1523) and how a durable write reaches the disk.
//!
//! It owns **no state**. Every function here is scoped to an explicit
//! `preferences.json` path, so the same code path serves production and
//! tests. The state that used to live beside these rules — the cache, the
//! write lock, the revision and the profile directory — belongs to
//! [`super::store::PreferencesStore`], which composes these primitives into
//! one owned profile.
//!
//! The bytes on disk are owned by [`super::recovery`]: it classifies a
//! payload, keeps the last-known-good backup, and performs the explicit
//! recovery actions. This module owns the *process* rules — what may be
//! written, and in what order — and delegates every byte to `recovery`. The
//! split is deliberate: `recovery::classify` is pure, so the classification
//! rules are testable without a filesystem, while the stateful decision
//! ("may this write land?") has exactly one home.
//!
//! See the [module-level docs](super) for what concerns each submodule owns.

use super::model::AppPreferences;
use super::recovery::{self, CorruptionInfo};
use std::path::Path;

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
pub(super) fn defaults() -> AppPreferences {
    let mut prefs = AppPreferences::default();
    super::launch_configurations::reconcile(&mut prefs);
    prefs
}

/// Read and classify the on-disk file. Never writes, never mutates a cache.
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
pub(crate) fn read_state(path: &Path, profile: &str) -> Result<LoadState, String> {
    read_state_flagging_plaintext(path, profile).map(|(state, _)| state)
}

/// [`read_state`] plus whether the file still held a plaintext API key
/// (issue #830) — the cue for the store to scrub the file.
///
/// `profile` is the credential namespace the file's secrets belong to. It
/// follows the store that owns the profile, so a read never hydrates a key
/// from another profile's vault.
pub(crate) fn read_state_flagging_plaintext(
    path: &Path,
    profile: &str,
) -> Result<(LoadState, bool), String> {
    let raw = match std::fs::read(path) {
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
        plaintext_in_file = super::secrets::hydrate_json(value, profile);
    }) {
        Ok(mut prefs) => {
            super::launch_configurations::reconcile(&mut prefs);
            Ok((LoadState::Healthy(Box::new(prefs)), plaintext_in_file))
        }
        Err(payload) => {
            let info = payload.into_info(path, recovery::backup_available(path));
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
/// [`super::store::health`] — so nothing in the frontend depends on this
/// wording.
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
/// the one place that does. Both writers ([`super::store::PreferencesStore::save`],
/// [`super::store::PreferencesStore::try_update`]) go through it, so there is
/// no second path into a durable write.
pub(crate) fn writable_state(path: &Path, profile: &str) -> Result<LoadState, String> {
    match read_state(path, profile)? {
        state @ (LoadState::Healthy(_) | LoadState::Missing) => Ok(state),
        LoadState::Corrupt(info) => Err(corruption_error(&info)),
    }
}
