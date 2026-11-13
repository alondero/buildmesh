//! Shared test fixtures for the `preferences` submodules.
//!
//! Issue #1482: the preferences state belongs to a [`PreferencesStore`] — one
//! handle per profile directory, owning the cached value, the revision and
//! the locks. A test that owns its state constructs a store over a temporary
//! directory and passes the handle in; it never mutates an ambient global to
//! achieve isolation.
//!
//! There is deliberately no "install a store for this thread" fixture: issue
//! #1482 forbids a test-only ambient override, and a thread-local slot would
//! not be inherited by a worker thread anyway. A test that needs a worker to
//! see the same preferences clones the handle and passes it in.

use super::store::PreferencesStore;
use std::path::{Path, PathBuf};

// Per-feature test files. Each tests a single concern; the cross-module
// fixtures live here.
//
// A module whose tests moved next to it keeps them in a sibling `*_tests.rs`
// (issue #2532): `resolver::accounts`, `resolver::catalog`, `resolver::harness`
// and `resolver::pairings` own theirs, so a scoped check that selects
// `preferences::resolver::<module>::` finds the tests that cover it.
mod compatibility_tests;
mod confirm_quit_tests;
mod default_provider_tests;
mod migrations_tests;
mod pairing_compat_tests;
mod recovery_tests;
mod secrets_tests;
mod storage_tests;
mod store_tests;

static TEST_DIR_COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The directory name a chosen `id` would claim.
///
/// The per-process token is part of the name because Windows recycles process
/// ids: `buildmesh-prefs-test-<pid>-<counter>` from a run three days ago
/// still matched the next run's, and the next run then *adopted* that run's
/// leftover profile directory — a `preferences.json` it had not written and
/// would read back as the current state (issue #2220). The token makes the
/// name a function of *this* process instance rather than of the pid alone.
pub(crate) fn profile_dir_name(id: usize) -> String {
    format!(
        "buildmesh-prefs-test_{}_{}_{}",
        std::process::id(),
        crate::env::test_helpers::process_token(),
        id
    )
}

/// A fresh profile directory path for one test.
///
/// The claim steps around a survivor instead of adopting it.
///
/// The path is handed back **absent**: some callers need the path to be a
/// *file* (the storage layer's "the app-data path is a file" case), so the
/// fixture leaves the path absent for them to fill.
pub(crate) fn test_dir() -> PathBuf {
    test_dir_for(TEST_DIR_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}

/// The profile directory path a chosen `id` would claim.
///
/// Split out from [`test_dir`] so a test can seed the directory that id would
/// claim and prove the claim steps around it.
pub(crate) fn test_dir_for(id: usize) -> PathBuf {
    crate::env::test_helpers::claim_dir_name(&std::env::temp_dir(), &profile_dir_name(id))
        .expect("claiming a fresh preferences profile directory must succeed")
}

/// A claimed profile directory that removes itself when the test ends.
///
/// The claim proves the name belongs to this run, and the removal rides out
/// the window in which Windows is still releasing a handle - so a test that
/// fails part-way through still runs the removal instead of leaving its
/// directory behind. The leftovers are what a later run collides with, and a
/// fixture that builds `%TEMP%\buildmesh_*` by hand has the same leak
/// whether or not it has failed yet (issue #2220).
pub(crate) struct ProfileDir(PathBuf);

impl ProfileDir {
    /// The claimed path. It is **absent** until the test creates it.
    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ProfileDir {
    fn drop(&mut self) {
        // The path holds a directory in the usual case, and a *file* in the
        // storage layer's "the app-data path is a file" case.
        if self.0.is_dir() {
            let _ = crate::env::test_helpers::remove_dir_all_with_retry(&self.0);
        } else {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}

/// A claimed profile directory that removes itself when the test ends.
pub(crate) fn profile_dir() -> ProfileDir {
    ProfileDir(test_dir())
}

/// Run `f` with a store over a fresh temporary directory.
///
/// The store is a real [`PreferencesStore`] — the same implementation
/// production uses — so the code under test runs against production's
/// synchronization. Only the *instance* is test-scoped, and the handle is
/// handed to the test explicitly.
pub(crate) fn with_store<F: FnOnce(&PreferencesStore)>(f: F) -> PathBuf {
    let tmp = test_dir();
    std::fs::create_dir_all(&tmp).unwrap();
    let store = PreferencesStore::new(tmp.clone());
    // The credential-store double is per-thread; a fresh vault keeps the
    // secrets a test seeds invisible to its siblings.
    super::secrets::test_support::reset();
    f(&store);
    super::secrets::test_support::reset();
    // A removal that loses the race with a just-released handle leaves the
    // directory behind, and a leftover is what a later run collides with
    // (issue #2220). The retry rides out that window.
    let _ = crate::env::test_helpers::remove_dir_all_with_retry(&tmp);
    tmp
}
