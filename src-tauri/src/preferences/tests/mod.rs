//! Shared test fixtures for the `preferences` submodules.
//!
//! Issue #1482: the preferences state belongs to a [`PreferencesStore`] — one
//! handle per profile directory, owning the cached value, the revision and
//! the locks. A test that owns its state constructs a store over a temporary
//! directory and passes the handle in; it never mutates an ambient global to
//! achieve isolation.
//!
//! [`with_temp_dir`] remains for the suites that drive *production* code
//! paths, which still read preferences through the free-function façade. The
//! store it installs is the production implementation — the thread-local slot
//! selects *which* store, not how it synchronizes.

use super::store::PreferencesStore;
use std::path::PathBuf;

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

pub(crate) fn test_dir() -> PathBuf {
    let id = TEST_DIR_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!("buildmesh-prefs-test-{}-{id}", std::process::id()))
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
    let _ = std::fs::remove_dir_all(&tmp);
    tmp
}

/// Run `f` with the ambient store pointed at a fresh temporary directory.
///
/// The migration seam for the suites that drive production code paths, which
/// read preferences through the free-function façade rather than through an
/// explicit handle. The installed store is the production implementation —
/// the thread-local slot selects *which* store, not how it synchronizes.
pub(crate) fn with_temp_dir<F: FnOnce(&PathBuf)>(f: F) -> PathBuf {
    let tmp = test_dir();
    std::fs::create_dir_all(&tmp).unwrap();

    super::store::init_for_tests(tmp.clone());

    f(&tmp);

    super::store::reset_for_tests();
    let _ = std::fs::remove_dir_all(&tmp);
    tmp
}
